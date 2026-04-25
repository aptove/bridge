use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing::{info, error, warn};

use bridge::cloudflare::{CloudflareClient, write_credentials_file, write_cloudflared_config, cloudflared_config_path};
use bridge::cloudflared_runner::CloudflaredRunner;
use bridge::bridge::StdioBridge;
use bridge::common_config::{self as common_config, CommonConfig, SlashCommandConfig, TransportConfig};
use bridge::config::{self as config, BridgeConfig};
use bridge::pairing::PairingManager;
use bridge::tls::TlsConfig;
use bridge::qr as qr;
use bridge::tailscale::{is_tailscale_available, is_tailscale_installed, get_tailscale_ipv4, get_tailscale_hostname, tailscale_serve_start, TailscaleServeGuard};
use bridge::agent_pool::{AgentPool, PoolConfig, start_reaper};

#[derive(Parser)]
#[command(name = "bridge", version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Bridge stdio-based ACP agents to mobile apps", long_about = None)]
#[command(subcommand_required = false, disable_version_flag = true)]
struct Cli {
    /// Print version
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
    version: (),

    /// Custom configuration directory (default: system config location)
    #[arg(short = 'c', long, global = true)]
    config_dir: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Set up Cloudflare Zero Trust infrastructure
    Setup {
        /// Your Cloudflare API token (with appropriate permissions)
        #[arg(short, long)]
        api_token: String,

        /// Your Cloudflare account ID
        #[arg(short = 'i', long)]
        account_id: String,

        /// Your domain managed by Cloudflare
        #[arg(short, long)]
        domain: String,

        /// Subdomain to use for the bridge (e.g., 'agent' for agent.yourdomain.com)
        #[arg(short, long, default_value = "agent")]
        subdomain: String,

        /// Tunnel name
        #[arg(short, long, default_value = "aptove-tunnel")]
        tunnel_name: String,
    },

    /// Run the bridge server (reads transport config from common.toml)
    Run {
        /// Command to run the ACP agent (e.g., "copilot --acp").
        /// If omitted, an interactive menu lets you pick a known agent.
        #[arg(short, long)]
        agent_command: Option<String>,

        /// Address to bind the WebSocket server
        #[arg(short, long, default_value = "0.0.0.0")]
        bind: String,

        /// Override the advertised LAN address in the QR code pairing URL.
        /// Useful when running in Docker or Apple Native containers with -p port
        /// forwarding, where the auto-detected IP is an internal virtual address.
        /// Example: --advertise-addr 192.168.1.50
        #[arg(long)]
        advertise_addr: Option<String>,

        /// Enable verbose logging (shows info level logs)
        #[arg(short, long)]
        verbose: bool,
    },

    /// Show connection QR code for a second device to connect to the running bridge
    ///
    /// Displays the QR code for the currently active transport. The bridge must
    /// already be running (via `bridge run`). Use this when you need to pair an
    /// additional device without restarting the bridge.
    ShowQr,

    /// Check configuration status
    Status,
}

/// Ensure Cloudflare config exists — load it if valid, auto-rotate token if near expiry,
/// or run interactive first-time setup. Reserved for potential future Start --cloudflare flag.
#[allow(dead_code)]
async fn ensure_cloudflare_config(no_auth: bool) -> Result<BridgeConfig> {
    use std::io::{self, BufRead, Write};

    // If a valid config already exists (has tunnel and service token), check token health
    if let Ok(mut cfg) = BridgeConfig::load() {
        if !cfg.tunnel_id.is_empty() && !cfg.client_id.is_empty() && !cfg.client_secret.is_empty() {
            if cfg.tunnel_secret.is_empty() {
                warn!("⚠️  Tunnel secret is missing from config — credentials are lost.");
                warn!("   Delete the config and re-run to trigger full re-setup:");
                warn!("   rm {}", BridgeConfig::config_path().display());
                anyhow::bail!("Tunnel secret lost. Delete config and re-run.");
            }
            let credentials_path = write_credentials_file(&cfg.account_id, &cfg.tunnel_id, &cfg.tunnel_secret)?;
            write_cloudflared_config(&cfg.tunnel_id, &credentials_path, cfg.hostname.trim_start_matches("https://"), 8080)?;

            if cfg.service_token_needs_rotation() {
                if cfg.api_token.is_empty() {
                    warn!("⚠️  Cloudflare service token is expiring soon but no API token is saved.");
                    warn!("   Delete the config file and re-run to trigger full re-setup:");
                    warn!("   rm {}", BridgeConfig::config_path().display());
                } else {
                    info!("🔄 Cloudflare service token is expiring — auto-rotating...");
                    let client = CloudflareClient::new(cfg.api_token.clone(), cfg.account_id.clone());
                    match client.create_service_token(&cfg.hostname.trim_start_matches("https://")).await {
                        Ok(new_token) => {
                            cfg.client_id = new_token.client_id;
                            cfg.client_secret = new_token.client_secret;
                            cfg.stamp_service_token_issued();
                            cfg.save()?;
                            info!("✅ Service token rotated — re-scan QR code on your mobile app");
                        }
                        Err(e) => {
                            warn!("⚠️  Service token rotation failed: {}. Using existing token.", e);
                        }
                    }
                }
            } else {
                info!("✅ Using existing Cloudflare configuration for {}", cfg.hostname);
            }
            if !no_auth && cfg.auth_token.is_empty() {
                cfg.ensure_auth_token();
                cfg.save()?;
            }
            return Ok(cfg);
        }
    }

    // No valid config — prompt the user interactively
    println!("\n🔧 Cloudflare Zero Trust is not configured yet. Let's set it up now.");
    println!("   (You only need to do this once — credentials are saved to disk.)\n");

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    let mut prompt = |msg: &str| -> Result<String> {
        print!("{}", msg);
        io::stdout().flush()?;
        Ok(lines.next().context("stdin closed")??.trim().to_string())
    };

    let api_token = prompt("  Cloudflare API Token (Zones:Edit + Access:*:Edit + Service Tokens:Edit): ")?;
    let account_id = prompt("  Cloudflare Account ID: ")?;
    let domain = prompt("  Domain (e.g. example.com): ")?;
    let subdomain_input = prompt("  Subdomain [agent]: ")?;
    let subdomain = if subdomain_input.is_empty() { "agent".to_string() } else { subdomain_input };
    let tunnel_name = format!("{}-tunnel", domain.split('.').next().unwrap_or("bridge"));

    println!();
    info!("🚀 Running Cloudflare Zero Trust setup...");

    let client = CloudflareClient::new(api_token.clone(), account_id.clone());
    let hostname = format!("{}.{}", subdomain, domain);

    info!("📡 Creating tunnel: {}", tunnel_name);
    let tunnel = client.create_or_get_tunnel(&tunnel_name).await?;
    info!("✅ Tunnel: {}", tunnel.id);

    info!("🌐 Configuring DNS record for {}", hostname);
    client.create_dns_record(&domain, &subdomain, &tunnel.id).await?;
    info!("✅ DNS record ready");

    info!("🔐 Creating Access Application...");
    let _app = client.create_access_application(&hostname).await?;
    info!("✅ Access Application ready");

    info!("🎫 Generating Service Token...");
    let service_token = client.create_service_token(&hostname).await?;
    info!("✅ Service Token created");

    info!("⚙️  Configuring tunnel ingress...");
    client.configure_tunnel_ingress(&tunnel.id, &hostname, 8080).await?;
    info!("✅ Tunnel ingress configured");

    let credentials_path = write_credentials_file(&account_id, &tunnel.id, &tunnel.secret)?;
    write_cloudflared_config(&tunnel.id, &credentials_path, &hostname, 8080)?;

    let mut cfg = BridgeConfig {
        hostname: format!("https://{}", hostname),
        tunnel_id: tunnel.id,
        tunnel_secret: tunnel.secret,
        account_id,
        client_id: service_token.client_id,
        client_secret: service_token.client_secret,
        domain,
        subdomain,
        auth_token: String::new(),
        cert_fingerprint: None,
        service_token_issued_at: None,
        api_token,
    };
    cfg.stamp_service_token_issued();
    if !no_auth {
        cfg.ensure_auth_token();
    }
    cfg.save()?;
    info!("✅ Configuration saved to: {}", BridgeConfig::config_path().display());

    Ok(cfg)
}

/// Run interactive Cloudflare Zero Trust setup, returning a ready `TransportConfig`.
///
/// Adapted from `ensure_cloudflare_config()` but returns a `TransportConfig` instead of
/// `BridgeConfig`, suitable for inserting into `CommonConfig.transports`.
async fn setup_cloudflare_transport() -> Result<TransportConfig> {
    use std::io::{self, BufRead, Write};

    println!("\n🔧 Cloudflare Zero Trust setup");
    println!("   (You only need to do this once — credentials are saved to disk.)\n");

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    let mut prompt = |msg: &str| -> Result<String> {
        print!("{}", msg);
        io::stdout().flush()?;
        Ok(lines.next().context("stdin closed")??.trim().to_string())
    };

    let api_token = prompt("  Cloudflare API Token (Zones:Edit + Access:*:Edit + Service Tokens:Edit): ")?;
    let account_id = prompt("  Cloudflare Account ID: ")?;
    let domain = prompt("  Domain (e.g. example.com): ")?;
    let subdomain_input = prompt("  Subdomain [agent]: ")?;
    let subdomain = if subdomain_input.is_empty() { "agent".to_string() } else { subdomain_input };
    let tunnel_name = format!("{}-tunnel", domain.split('.').next().unwrap_or("bridge"));

    println!();
    info!("🚀 Running Cloudflare Zero Trust setup...");

    let client = CloudflareClient::new(api_token, account_id.clone());
    let hostname = format!("{}.{}", subdomain, domain);

    info!("📡 Creating tunnel: {}", tunnel_name);
    let tunnel = client.create_or_get_tunnel(&tunnel_name).await?;
    info!("✅ Tunnel: {}", tunnel.id);

    info!("🌐 Configuring DNS record for {}", hostname);
    client.create_dns_record(&domain, &subdomain, &tunnel.id).await?;
    info!("✅ DNS record ready");

    info!("🔐 Creating Access Application...");
    let _app = client.create_access_application(&hostname).await?;
    info!("✅ Access Application ready");

    info!("🎫 Generating Service Token...");
    let service_token = client.create_service_token(&hostname).await?;
    info!("✅ Service Token created");

    info!("⚙️  Configuring tunnel ingress...");
    client.configure_tunnel_ingress(&tunnel.id, &hostname, 8080).await?;
    info!("✅ Tunnel ingress configured");

    let credentials_path = write_credentials_file(&account_id, &tunnel.id, &tunnel.secret)?;
    write_cloudflared_config(&tunnel.id, &credentials_path, &hostname, 8080)?;

    info!("✅ Cloudflare setup complete for {}", hostname);

    Ok(TransportConfig {
        enabled: true,
        port: Some(8080),
        tls: None,
        hostname: Some(format!("https://{}", hostname)),
        tunnel_id: Some(tunnel.id),
        tunnel_secret: Some(tunnel.secret),
        account_id: Some(account_id),
        client_id: Some(service_token.client_id),
        client_secret: Some(service_token.client_secret),
        domain: Some(domain),
        subdomain: Some(subdomain),
    })
}

/// Build a `PairingManager` and optionally a `TlsConfig` for a single transport.
///
/// Returns `(hostname, pairing_manager, tls_config, extra_guards)` where
/// `extra_guards` holds any external daemon handles that must stay alive.
fn build_transport(
    transport_name: &str,
    transport_cfg: &TransportConfig,
    common: &CommonConfig,
    config_dir: &std::path::PathBuf,
    advertise_addr: Option<&str>,
    cwd: &str,
) -> Result<(String, PairingManager, Option<TlsConfig>, Option<TailscaleServeGuard>, Option<CloudflaredRunner>)> {
    // tailscale-serve binds to localhost only and needs its own port so it doesn't
    // conflict with the local transport that may also be active on 8765.
    let default_port: u16 = if transport_name == "tailscale-serve" { 8766 } else { 8765 };
    let port = transport_cfg.port.unwrap_or(default_port);
    let use_tls = transport_cfg.tls.unwrap_or(true);

    match transport_name {
        "cloudflare" => {
            let hostname = transport_cfg
                .hostname
                .clone()
                .unwrap_or_default();
            let client_id = transport_cfg.client_id.clone();
            let client_secret = transport_cfg.client_secret.clone();

            let pm = PairingManager::new_with_cf(
                common.agent_id.clone(),
                hostname.clone(),
                common.auth_token.clone(),
                None,
                client_id,
                client_secret,
                cwd.to_string(),
            );

            // Start cloudflared
            let tunnel_id = transport_cfg.tunnel_id.clone().unwrap_or_default();
            let runner = if !tunnel_id.is_empty() {
                let config_yml = cloudflared_config_path()?;
                info!("🌐 Starting cloudflared tunnel daemon...");
                let mut runner = CloudflaredRunner::spawn(&config_yml, &tunnel_id)?;
                runner.wait_for_ready(std::time::Duration::from_secs(30))?;
                println!("🌐 Cloudflare tunnel active: {}", hostname);
                Some(runner)
            } else {
                warn!("Cloudflare transport: tunnel_id not configured, skipping cloudflared startup");
                None
            };

            Ok((hostname, pm, None, None, runner))
        }

        "tailscale-serve" => {
            let ts_hostname = get_tailscale_hostname()?
                .ok_or_else(|| anyhow::anyhow!(
                    "tailscale-serve mode requires MagicDNS + HTTPS to be enabled on your tailnet.\n\
                     Enable HTTPS in the Tailscale admin console: https://tailscale.com/kb/1153/enabling-https"
                ))?;
            // Tailscale serve always exposes on port 443, so no port suffix in the URL.
            let hostname = format!("wss://{}", ts_hostname);

            let pm = PairingManager::new_with_cf(
                common.agent_id.clone(),
                hostname.clone(),
                common.auth_token.clone(),
                None,
                None,
                None,
                cwd.to_string(),
            ).with_tailscale_path();

            info!("🌐 Starting tailscale serve...");
            let guard = tailscale_serve_start(port)?;
            println!("📡 Tailscale (serve): wss://{}", ts_hostname);

            Ok((hostname, pm, None, Some(guard), None))
        }

        _ => {
            // "local" and any unknown transports — local network with self-signed TLS.
            // Include the advertise_addr in the cert SANs so iOS TLS validation passes
            // when connecting via the LAN IP (e.g. from a container with --advertise-addr).
            let extra_sans: Vec<String> = advertise_addr
                .map(|a| vec![a.to_string()])
                .unwrap_or_default();
            let tls_config = if use_tls {
                Some(TlsConfig::load_or_generate(config_dir, &extra_sans)?)
            } else {
                None
            };
            let cert_fingerprint = tls_config.as_ref().map(|t| t.fingerprint.clone());
            let ip = match advertise_addr {
                Some(addr) => addr.to_string(),
                None => match local_ip_address::local_ip() {
                    Ok(addr) => addr.to_string(),
                    Err(_) => "127.0.0.1".to_string(),
                },
            };
            let protocol = if tls_config.is_some() { "wss" } else { "ws" };
            let hostname = format!("{}://{}:{}", protocol, ip, port);

            let pm = PairingManager::new_with_cf(
                common.agent_id.clone(),
                hostname.clone(),
                common.auth_token.clone(),
                cert_fingerprint,
                None,
                None,
                cwd.to_string(),
            );

            Ok((hostname, pm, tls_config, None, None))
        }
    }
}

/// Probe each enabled transport's listen port to find which one is currently active.
/// Returns the first transport whose port accepts a TCP connection.
fn find_active_transport(config: &CommonConfig) -> Option<(String, TransportConfig)> {
    for (name, cfg) in config.enabled_transports() {
        let default_port: u16 = if name == "tailscale-serve" { 8766 } else { 8765 };
        let port = cfg.port.unwrap_or(default_port);
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        if std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300))
            .is_ok()
        {
            return Some((name.to_string(), cfg.clone()));
        }
    }
    None
}

/// Build and display a static connection QR for the given transport.
///
/// Reads credentials from `config` and the TLS cert from disk. No server is started.
fn show_static_qr(
    transport_name: &str,
    transport_cfg: &TransportConfig,
    config: &CommonConfig,
    config_dir: &std::path::PathBuf,
) -> Result<()> {
    use serde_json::{Map, Value};

    let default_port: u16 = if transport_name == "tailscale-serve" { 8766 } else { 8765 };
    let port = transport_cfg.port.unwrap_or(default_port);

    let mut map = Map::new();
    if !config.agent_id.is_empty() {
        map.insert("agentId".to_string(), Value::String(config.agent_id.clone()));
    }
    map.insert("protocol".to_string(), Value::String("acp".to_string()));
    map.insert("version".to_string(), Value::String("1.0".to_string()));
    if !config.auth_token.is_empty() {
        map.insert("authToken".to_string(), Value::String(config.auth_token.clone()));
    }

    match transport_name {
        "cloudflare" => {
            let hostname = transport_cfg.hostname.clone().unwrap_or_default();
            let url = hostname.replacen("https://", "wss://", 1);
            map.insert("url".to_string(), Value::String(url));
            if let Some(id) = transport_cfg.client_id.as_deref().filter(|s| !s.is_empty()) {
                map.insert("clientId".to_string(), Value::String(id.to_string()));
            }
            if let Some(secret) = transport_cfg.client_secret.as_deref().filter(|s| !s.is_empty()) {
                map.insert("clientSecret".to_string(), Value::String(secret.to_string()));
            }
        }
        "tailscale-serve" => {
            let ts_hostname = get_tailscale_hostname()?
                .ok_or_else(|| anyhow::anyhow!("Tailscale MagicDNS hostname not available"))?;
            map.insert("url".to_string(), Value::String(format!("wss://{}:{}", ts_hostname, port)));
        }
        _ => {
            // "local" and any unknown name
            let tls_config = TlsConfig::load_or_generate(config_dir, &[])?;
            let ip = match local_ip_address::local_ip() {
                Ok(a) => a.to_string(),
                Err(_) => "127.0.0.1".to_string(),
            };
            map.insert("url".to_string(), Value::String(format!("wss://{}:{}", ip, port)));
            map.insert("certFingerprint".to_string(), Value::String(tls_config.fingerprint));
        }
    }

    let json = serde_json::to_string(&Value::Object(map))?;
    qr::display_qr_code(&json, transport_name)?;
    Ok(())
}

/// Validate that the agent command binary exists and is executable before starting the bridge.
/// Provides a clear error at startup rather than a cryptic failure when the first client connects.
fn validate_agent_command(command: &str) -> Result<()> {
    let binary = command
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Agent command is empty"))?;

    let binary_path = std::path::Path::new(binary);

    if binary_path.is_absolute() || binary.contains('/') {
        // Explicit path — check it directly without PATH lookup
        anyhow::ensure!(
            binary_path.exists(),
            "Agent binary not found: {}",
            binary
        );
        anyhow::ensure!(
            binary_path.is_file(),
            "Agent path is not a regular file: {}",
            binary
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(binary_path)?.permissions().mode();
            anyhow::ensure!(
                mode & 0o111 != 0,
                "Agent binary is not executable: {}",
                binary
            );
        }
    } else {
        // Bare name — resolve against PATH
        which::which(binary).with_context(|| {
            format!(
                "Agent binary '{}' not found in PATH. Is it installed and on your PATH?",
                binary
            )
        })?;
    }

    Ok(())
}

fn prompt_agent_command() -> Result<String> {
    use std::io::Write as _;

    const AGENTS: &[(&str, &str)] = &[
        ("Copilot", "copilot --acp"),
        ("Gemini", "gemini --experimental-acp"),
        ("Goose", "goose acp"),
    ];

    println!("\nSelect an agent to run:");
    for (i, (name, cmd)) in AGENTS.iter().enumerate() {
        println!("  [{}] {}  ({})", i + 1, name, cmd);
    }
    println!("  [{}] Custom", AGENTS.len() + 1);
    print!("Enter number [1]: ");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let choice: usize = input.trim().parse().unwrap_or(1);

    if choice >= 1 && choice <= AGENTS.len() {
        Ok(AGENTS[choice - 1].1.to_string())
    } else {
        print!("Enter agent command: ");
        std::io::stdout().flush()?;
        let mut cmd = String::new();
        std::io::stdin().read_line(&mut cmd)?;
        let cmd = cmd.trim().to_string();
        if cmd.is_empty() {
            anyhow::bail!("No agent command provided");
        }
        Ok(cmd)
    }
}

/// Interactively select a transport, auto-configuring or running inline setup if needed.
///
/// Always shows all four transport options with a status indicator. If the chosen
/// transport is not yet configured in `config.transports`, it is created on the fly
/// and `config` is mutated + saved.
fn prompt_port(default: u16) -> Result<u16> {
    use std::io::Write as _;
    print!("  Enter port [{}]: ", default);
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(default)
    } else {
        trimmed.parse::<u16>().map_err(|_| anyhow::anyhow!("Invalid port number"))
    }
}

async fn prompt_transport_selection(config: &mut CommonConfig) -> Result<(String, TransportConfig)> {
    use std::io::Write as _;

    const TRANSPORTS: &[(&str, &str)] = &[
        ("local",           "Local Bridge Server"),
        ("tailscale-serve", "Tailscale (Recommended)"),
        ("cloudflare",      "Cloudflare Zero Trust"),
    ];

    loop {
        println!("\nSelect a transport:");
        for (i, &(internal, display)) in TRANSPORTS.iter().enumerate() {
            let status = match config.transports.get(internal) {
                Some(t) if t.enabled => "[ready]".to_string(),
                _ => match internal {
                    "local" => "[will auto-configure]".to_string(),
                    "tailscale-serve" => {
                        if is_tailscale_available() {
                            "[will enable]".to_string()
                        } else if is_tailscale_installed() {
                            "[Tailscale not running]".to_string()
                        } else {
                            "[not installed]".to_string()
                        }
                    }
                    "cloudflare" => "[setup required]".to_string(),
                    _ => String::new(),
                },
            };
            println!("  [{}] {:<30} {}", i + 1, display, status);
        }
        print!("Enter number [1]: ");
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let choice: usize = input.trim().parse().unwrap_or(1);
        let idx = if choice >= 1 && choice <= TRANSPORTS.len() {
            choice - 1
        } else {
            0
        };

        let (internal_name, _display_name) = TRANSPORTS[idx];

        // Not configured — handle per-transport auto-configuration.
        match internal_name {
            "local" => {
                let default_port = config.transports.get("local")
                    .and_then(|t| t.port)
                    .unwrap_or(8765);
                let port = if std::net::TcpListener::bind(format!("0.0.0.0:{}", default_port)).is_ok() {
                    println!("  Using port {}", default_port);
                    default_port
                } else {
                    println!("  ⚠️  Port {} is already in use.", default_port);
                    prompt_port(default_port.saturating_add(1))?
                };
                let tc = TransportConfig {
                    enabled: true,
                    port: Some(port),
                    tls: Some(true),
                    ..Default::default()
                };
                config.transports.insert("local".to_string(), tc.clone());
                config.save()?;
                println!("  ✅ Local transport configured (port {}, TLS on)", port);
                return Ok(("local".to_string(), tc));
            }
            "tailscale-serve" => {
                if !is_tailscale_available() {
                    println!("\n  ⚠️  Tailscale is not running. Start Tailscale and try again.\n");
                    continue;
                }
                let default_port = config.transports.get("tailscale-serve")
                    .and_then(|t| t.port)
                    .unwrap_or(8766);
                let port = if std::net::TcpListener::bind(format!("0.0.0.0:{}", default_port)).is_ok() {
                    println!("  Using port {}", default_port);
                    default_port
                } else {
                    println!("  ⚠️  Port {} is already in use.", default_port);
                    prompt_port(default_port.saturating_add(1))?
                };
                let tc = TransportConfig {
                    enabled: true,
                    port: Some(port),
                    tls: None,
                    ..Default::default()
                };
                config.transports.insert("tailscale-serve".to_string(), tc.clone());
                config.save()?;
                println!("  ✅ Tailscale Serve transport configured (port {})", port);
                return Ok(("tailscale-serve".to_string(), tc));
            }
            "cloudflare" => {
                let tc = setup_cloudflare_transport().await?;
                config.transports.insert("cloudflare".to_string(), tc.clone());
                config.save()?;
                return Ok(("cloudflare".to_string(), tc));
            }
            _ => unreachable!(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Set custom config directory if provided (both old and new config systems)
    if let Some(ref dir) = cli.config_dir {
        config::set_config_dir(dir.clone());
        common_config::set_config_dir(dir.clone());
    }

    let command = cli.command.unwrap_or(Commands::Run {
        agent_command: None,
        bind: "0.0.0.0".to_string(),
        advertise_addr: None,
        verbose: false,
    });

    // Determine log level based on command and flags
    let log_level = match &command {
        Commands::Run { verbose, .. } => {
            if *verbose { "info" } else { "warn" }
        }
        _ => "info",
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level))
        )
        .init();

    match command {
        Commands::Setup {
            api_token,
            account_id,
            domain,
            subdomain,
            tunnel_name,
        } => {
            info!("🚀 Starting Cloudflare Zero Trust setup...");

            let client = CloudflareClient::new(api_token.clone(), account_id.clone());

            info!("📡 Creating Cloudflare Tunnel: {}", tunnel_name);
            let tunnel = client.create_or_get_tunnel(&tunnel_name).await?;
            info!("✅ Tunnel created: {}", tunnel.id);

            let hostname = format!("{}.{}", subdomain, domain);
            info!("🌐 Creating DNS record for: {}", hostname);
            client.create_dns_record(&domain, &subdomain, &tunnel.id).await?;
            info!("✅ DNS record created");

            info!("🔐 Creating Zero Trust Access Application...");
            let app = client.create_access_application(&hostname).await?;
            info!("✅ Access Application created: {}", app.id);

            info!("🎫 Generating Service Token...");
            let service_token = client.create_service_token(&hostname).await?;
            info!("✅ Service Token created");

            info!("⚙️  Configuring tunnel ingress rules...");
            client.configure_tunnel_ingress(&tunnel.id, &hostname, 8080).await?;
            info!("✅ Tunnel ingress configured");

            info!("📄 Writing cloudflared credentials file...");
            let credentials_path = write_credentials_file(&account_id, &tunnel.id, &tunnel.secret)
                .context("Failed to write cloudflared credentials file")?;
            info!("✅ Credentials file written to: {}", credentials_path.display());

            info!("📄 Writing cloudflared config.yml...");
            let config_yml_path = write_cloudflared_config(&tunnel.id, &credentials_path, &hostname, 8080)
                .context("Failed to write cloudflared config.yml")?;
            info!("✅ Config file written to: {}", config_yml_path.display());

            let mut config = BridgeConfig {
                hostname: format!("https://{}", hostname),
                tunnel_id: tunnel.id.clone(),
                tunnel_secret: tunnel.secret.clone(),
                account_id,
                client_id: service_token.client_id,
                client_secret: service_token.client_secret,
                domain,
                subdomain,
                auth_token: String::new(),
                cert_fingerprint: None,
                service_token_issued_at: None,
                api_token,
            };

            config.ensure_auth_token();
            config.stamp_service_token_issued();
            config.save()?;
            info!("✅ Configuration saved to: {}", BridgeConfig::config_path().display());

            println!("\n🎉 Setup complete!\n");
            let json = config.to_connection_json()?;
            qr::display_qr_code(&json, "cloudflare")?;
            println!("\n⚠️  Important: Keep your configuration file secure. It contains sensitive credentials.");
            println!("\n🚀 Start the bridge with: bridge");
        }

        Commands::Run { agent_command, bind, advertise_addr, verbose: _ } => {
            let agent_command = match agent_command {
                Some(cmd) => cmd,
                None => prompt_agent_command()?,
            };

            validate_agent_command(&agent_command)?;

            info!("🌉 Starting ACP Bridge v{}", env!("CARGO_PKG_VERSION"));

            // Load (or initialise) the common config
            let mut config = CommonConfig::load()?;
            config.ensure_agent_id();
            config.ensure_auth_token();
            config.save()?;

            let (transport_name, mut transport_cfg) = prompt_transport_selection(&mut config).await?;

            let config_dir = CommonConfig::config_dir();

            // tailscale-serve proxies from Tailscale edge → localhost, so bind
            // to 127.0.0.1 only; all other transports use the user-supplied bind addr.
            let effective_bind = if transport_name == "tailscale-serve" {
                "127.0.0.1".to_string()
            } else {
                bind.clone()
            };

            // Ensure the chosen port is free; re-prompt if taken.
            loop {
                let default_port: u16 = if transport_name == "tailscale-serve" { 8766 } else { 8765 };
                let port = transport_cfg.port.unwrap_or(default_port);
                let check_addr = format!("{}:{}", effective_bind, port);
                if std::net::TcpListener::bind(&check_addr).is_ok() {
                    break;
                }
                println!("\n  ⚠️  Port {} is already in use. Choose a different port.", port);
                let new_port = prompt_port(port.saturating_add(1))?;
                transport_cfg.port = Some(new_port);
                if let Some(t) = config.transports.get_mut(&transport_name) {
                    t.port = Some(new_port);
                }
                config.save()?;
            }

            // tailscale-serve defaults to 8766 to avoid conflicting with local (8765).
            let default_port: u16 = if transport_name == "tailscale-serve" { 8766 } else { 8765 };
            let port = transport_cfg.port.unwrap_or(default_port);

            let cwd = std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .to_string_lossy()
                .to_string();

            let (hostname, pm, tls_config, _ts_guard, _cf_runner) =
                build_transport(&transport_name, &transport_cfg, &config, &config_dir, advertise_addr.as_deref(), &cwd)?;

            if transport_name == "cloudflare" {
                let json = config.to_connection_json(&hostname, &transport_name, &cwd)?;
                qr::display_qr_code(&json, &transport_name)?;
            } else {
                qr::display_qr_code_with_pairing(&hostname, &pm)?;
            }

            info!("📡 Starting WebSocket server on {}:{} (transport: {})", effective_bind, port, transport_name);
            info!("🤖 Agent command: {}", agent_command);

            let uses_external_tls = matches!(transport_name.as_str(), "tailscale-serve" | "cloudflare");

            let mut bridge = StdioBridge::new(agent_command.clone(), port)
                .with_bind_addr(effective_bind)
                .with_auth_token(Some(config.auth_token.clone()))
                .with_pairing(pm);

            if let Some(tls) = tls_config {
                info!("🔒 TLS fingerprint: {}", tls.fingerprint_short());
                bridge = bridge.with_tls(tls);
            } else if uses_external_tls {
                bridge = bridge.with_external_tls();
            }

            // Enable agent pool for persistent sessions (keep-alive across disconnects)
            let pool = std::sync::Arc::new(tokio::sync::RwLock::new(
                AgentPool::new(PoolConfig::default())
                    .with_working_dir(cwd.clone().into()),
            ));
            let _reaper = start_reaper(pool.clone(), std::time::Duration::from_secs(60));
            bridge = bridge.with_agent_pool(pool);
            info!("♻️  Agent pool enabled (idle timeout: 30m, max agents: 10)");

            // Inject slash commands for agents that don't send
            // available_commands_update themselves (e.g. Copilot CLI).
            // Use commands from common.toml if configured, otherwise built-in defaults.
            let slash_commands = if config.slash_commands.is_empty() {
                vec![
                    SlashCommandConfig { name: "help".into(), description: "Show available commands and usage".into(), input_hint: None },
                    SlashCommandConfig { name: "clear".into(), description: "Clear the conversation history".into(), input_hint: None },
                    SlashCommandConfig { name: "compact".into(), description: "Compact conversation history, optionally with a focus topic".into(), input_hint: Some("focus topic (optional)".into()) },
                    SlashCommandConfig { name: "agent".into(), description: "Configure agent settings".into(), input_hint: None },
                ]
            } else {
                config.slash_commands.clone()
            };
            info!("📋 {} slash command(s) configured for injection", slash_commands.len());
            bridge = bridge.with_slash_commands(slash_commands);

            // _ts_guard, _cf_runner, and _reaper live until end of this block (bridge lifetime).
            match bridge.start().await {
                Ok(()) => info!("Bridge exited cleanly"),
                Err(e) => error!("Bridge error: {}", e),
            }
        }

        Commands::ShowQr => {
            let config = CommonConfig::load()?;
            let config_dir = CommonConfig::config_dir();

            match find_active_transport(&config) {
                Some((transport_name, transport_cfg)) => {
                    show_static_qr(&transport_name, &transport_cfg, &config, &config_dir)?;
                }
                None => {
                    println!("Bridge is not running.");
                    println!();
                    println!("Start it first, then run 'bridge show-qr' to display the connection QR:");
                    println!("  bridge");
                }
            }
        }

        Commands::Status => {
            // Show CommonConfig status
            match CommonConfig::load() {
                Ok(config) => {
                    println!("✅ CommonConfig found\n");
                    println!("Agent ID:   {}", if config.agent_id.is_empty() { "(not yet assigned)" } else { &config.agent_id });
                    println!("Config:     {}", CommonConfig::config_path().display());
                    println!();
                    println!("Transports:");
                    if config.transports.is_empty() {
                        println!("  (none configured)");
                    }
                    let mut names: Vec<_> = config.transports.keys().collect();
                    names.sort();
                    for name in names {
                        let t = &config.transports[name];
                        let status = if t.enabled { "enabled" } else { "disabled" };
                        let port_str = t.port.map(|p| format!(":{}", p)).unwrap_or_default();
                        println!("  {:<20} {} {}", name, status, port_str);
                    }
                }
                Err(e) => {
                    error!("❌ No CommonConfig found: {}", e);
                }
            }

            // Check Tailscale status
            if is_tailscale_available() {
                match get_tailscale_ipv4() {
                    Ok(ip) => {
                        println!("Tailscale IP: {}", ip);
                        if let Ok(Some(hostname)) = get_tailscale_hostname() {
                            println!("Tailscale hostname: {}", hostname);
                        }
                    }
                    Err(_) => println!("Tailscale: not enrolled (run 'tailscale up')"),
                }
            } else if is_tailscale_installed() {
                println!("Tailscale: installed but not running (open the Tailscale app to connect)");
            } else {
                println!("Tailscale: not installed (https://tailscale.com/download)");
            }
        }
    }

    Ok(())
}

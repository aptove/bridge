use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{info, error, warn};

use bridge::cloudflare::{CloudflareClient, write_credentials_file, write_cloudflared_config, cloudflared_config_path};
use bridge::cloudflared_runner::CloudflaredRunner;
use bridge::bridge::StdioBridge;
use bridge::common_config::{self as common_config, CommonConfig, TransportConfig};
use bridge::config::{self as config, BridgeConfig};
use bridge::pairing::{PairingManager, PairingError};
use bridge::tls::TlsConfig;
use bridge::qr as qr;
use bridge::tailscale::{is_tailscale_available, get_tailscale_ipv4, get_tailscale_hostname, tailscale_serve_start, TailscaleServeGuard};

#[derive(Parser)]
#[command(name = "bridge")]
#[command(about = "Bridge stdio-based ACP agents to mobile apps", long_about = None)]
struct Cli {
    /// Custom configuration directory (default: system config location)
    #[arg(long, global = true)]
    config_dir: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Commands,
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

    /// Start the bridge server (reads transport config from common.toml)
    Start {
        /// Command to run the ACP agent (e.g., "gemini --experimental-acp")
        #[arg(short, long)]
        agent_command: String,

        /// Address to bind the WebSocket server
        #[arg(short, long, default_value = "0.0.0.0")]
        bind: String,

        /// Show QR code for mobile connection at startup
        #[arg(short = 'Q', long)]
        qr: bool,

        /// Enable verbose logging (shows info level logs)
        #[arg(short, long)]
        verbose: bool,
    },

    /// Show connection QR code (detects whether bridge is running)
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

/// Build a `PairingManager` and optionally a `TlsConfig` for a single transport.
///
/// Returns `(hostname, pairing_manager, tls_config, extra_guards)` where
/// `extra_guards` holds any external daemon handles that must stay alive.
fn build_transport(
    transport_name: &str,
    transport_cfg: &TransportConfig,
    common: &CommonConfig,
    config_dir: &std::path::PathBuf,
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
            let hostname = format!("wss://{}", ts_hostname);

            let pm = PairingManager::new_with_cf(
                common.agent_id.clone(),
                hostname.clone(),
                common.auth_token.clone(),
                None,
                None,
                None,
            ).with_tailscale_path();

            info!("🌐 Starting tailscale serve...");
            let guard = tailscale_serve_start(port)?;
            println!("📡 Tailscale (serve): wss://{}", ts_hostname);

            Ok((hostname, pm, None, Some(guard), None))
        }

        "tailscale-ip" => {
            let ts_ip = get_tailscale_ipv4()?;
            let addr = get_tailscale_hostname()?
                .unwrap_or_else(|| ts_ip.clone());
            let extra_sans = vec![ts_ip];

            let tls_config = if use_tls {
                Some(TlsConfig::load_or_generate(config_dir, &extra_sans)?)
            } else {
                None
            };
            let cert_fingerprint = tls_config.as_ref().map(|t| t.fingerprint.clone());
            let protocol = if tls_config.is_some() { "wss" } else { "ws" };
            let hostname = format!("{}://{}:{}", protocol, addr, port);
            println!("📡 Tailscale (ip): {}", hostname);

            let pm = PairingManager::new_with_cf(
                common.agent_id.clone(),
                hostname.clone(),
                common.auth_token.clone(),
                cert_fingerprint,
                None,
                None,
            ).with_tailscale_path();

            Ok((hostname, pm, tls_config, None, None))
        }

        _ => {
            // "local" and any unknown transports — local network with self-signed TLS
            let tls_config = if use_tls {
                Some(TlsConfig::load_or_generate(config_dir, &[])?)
            } else {
                None
            };
            let cert_fingerprint = tls_config.as_ref().map(|t| t.fingerprint.clone());
            let ip = match local_ip_address::local_ip() {
                Ok(addr) => addr.to_string(),
                Err(_) => "127.0.0.1".to_string(),
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
            );

            Ok((hostname, pm, tls_config, None, None))
        }
    }
}

/// Build a new `PairingManager` for a transport without restarting daemons.
fn make_pairing_manager(
    transport_name: &str,
    transport_cfg: &TransportConfig,
    common: &CommonConfig,
    hostname: &str,
    cert_fingerprint: Option<String>,
) -> PairingManager {
    match transport_name {
        "cloudflare" => PairingManager::new_with_cf(
            common.agent_id.clone(),
            hostname.to_string(),
            common.auth_token.clone(),
            None,
            transport_cfg.client_id.clone(),
            transport_cfg.client_secret.clone(),
        ),
        "tailscale-serve" | "tailscale-ip" => PairingManager::new_with_cf(
            common.agent_id.clone(),
            hostname.to_string(),
            common.auth_token.clone(),
            cert_fingerprint,
            None,
            None,
        )
        .with_tailscale_path(),
        _ => PairingManager::new_with_cf(
            common.agent_id.clone(),
            hostname.to_string(),
            common.auth_token.clone(),
            cert_fingerprint,
            None,
            None,
        ),
    }
}

/// Offline registration: start a pairing-only server so the mobile app can register
/// without the full bridge running. Loops to regenerate the QR on code expiry.
async fn run_offline_registration(config: &CommonConfig) -> Result<()> {
    let enabled: Vec<(String, TransportConfig)> = config
        .enabled_transports()
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();

    if enabled.is_empty() {
        anyhow::bail!(
            "No transports are enabled in common.toml.\n\
             Add at least one transport section with `enabled = true`."
        );
    }

    // Select transport — prompt when multiple are available
    let (transport_name, transport_cfg) = if enabled.len() == 1 {
        enabled[0].clone()
    } else {
        println!("\nSelect a transport for offline registration:");
        for (i, (name, _)) in enabled.iter().enumerate() {
            println!("  [{}] {}", i + 1, name);
        }
        print!("Enter number [1]: ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let choice: usize = input.trim().parse().unwrap_or(1);
        let idx = choice.saturating_sub(1).min(enabled.len() - 1);
        enabled[idx].clone()
    };

    let config_dir = CommonConfig::config_dir();
    let port = transport_cfg.port.unwrap_or(8765);

    // Build transport infrastructure once (TLS cert, hostname, optional daemons)
    let (hostname, _pm_initial, tls_config, _ts_guard, _cf_runner) =
        build_transport(&transport_name, &transport_cfg, config, &config_dir)?;
    let tls_arc = tls_config.map(Arc::new);
    let cert_fingerprint = tls_arc.as_ref().map(|t| t.fingerprint.clone());

    println!("\n📡 Offline registration mode — transport: {}", transport_name);
    println!("   Mobile app can register now; bridge start is not required yet.\n");

    loop {
        // Create a fresh PairingManager (new 6-digit code each iteration)
        let pm = Arc::new(make_pairing_manager(
            &transport_name,
            &transport_cfg,
            config,
            &hostname,
            cert_fingerprint.clone(),
        ));

        // Display QR code
        qr::display_qr_code_with_pairing(&hostname, &pm)?;

        // Bind TCP listener
        let addr = format!("0.0.0.0:{}", port);
        let listener = TcpListener::bind(&addr)
            .await
            .with_context(|| format!("Failed to bind pairing server on {}", addr))?;
        info!("🔗 Pairing server listening on {} for offline registration", addr);

        // Accept connections until pairing succeeds or code expires
        let paired = run_pairing_server(listener, Arc::clone(&pm), tls_arc.clone()).await;

        if paired {
            println!("\n✅ Registration complete!");
            println!("   Start the bridge whenever you're ready:");
            println!("   bridge start --agent-command \"<your-agent-command>\"");
            break;
        }

        // Code expired — offer to regenerate
        println!("\n⏰ Pairing code expired.");
        print!("   Generate a new QR? [Y/n]: ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let answer = input.trim().to_ascii_lowercase();
        if answer == "n" || answer == "no" {
            break;
        }
        println!();
    }

    Ok(())
}

/// Accept connections on `listener` until a pairing succeeds or the code expires.
/// Returns `true` if pairing was completed, `false` on expiry.
async fn run_pairing_server(
    listener: TcpListener,
    pm: Arc<PairingManager>,
    tls_config: Option<Arc<TlsConfig>>,
) -> bool {
    loop {
        if pm.is_expired() {
            return false;
        }
        if pm.is_used() {
            return true;
        }

        let remaining = pm.seconds_remaining();
        let accept_timeout = Duration::from_secs(remaining.min(2) + 1);

        match tokio::time::timeout(accept_timeout, listener.accept()).await {
            Ok(Ok((stream, addr))) => {
                info!("📱 Offline pairing connection from {}", addr);
                let pm_clone = Arc::clone(&pm);
                let tls_clone = tls_config.clone();
                let paired = handle_offline_connection(stream, pm_clone, tls_clone).await;
                if paired {
                    return true;
                }
            }
            Ok(Err(e)) => {
                error!("Accept error on pairing server: {}", e);
                return false;
            }
            Err(_) => {
                // Poll again — expiry check at top of loop
            }
        }
    }
}

/// Dispatch a raw TCP stream to the pairing handler, applying TLS if configured.
async fn handle_offline_connection(
    stream: tokio::net::TcpStream,
    pm: Arc<PairingManager>,
    tls_config: Option<Arc<TlsConfig>>,
) -> bool {
    if let Some(tls) = tls_config {
        match tls.acceptor.accept(stream).await {
            Ok(tls_stream) => handle_offline_pairing(tls_stream, pm).await,
            Err(e) => {
                warn!("TLS handshake failed on offline pairing: {}", e);
                false
            }
        }
    } else {
        handle_offline_pairing(stream, pm).await
    }
}

/// Read one HTTP request and respond to a pairing endpoint.
/// Returns `true` if pairing was accepted (code was valid).
async fn handle_offline_pairing<S>(mut stream: S, pm: Arc<PairingManager>) -> bool
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let mut buf = vec![0u8; 8192];
    let n = match stream.read(&mut buf).await {
        Ok(n) => n,
        Err(_) => return false,
    };
    let request = String::from_utf8_lossy(&buf[..n]);
    let first_line = request.lines().next().unwrap_or("");

    let is_pair_request = first_line.starts_with("GET")
        && (first_line.contains("/pair/local")
            || first_line.contains("/pair/cloudflare")
            || first_line.contains("/pair/tailscale"));

    if !is_pair_request {
        let resp = http_response(404, "Not Found", r#"{"error":"not_found"}"#);
        let _ = stream.write_all(resp.as_bytes()).await;
        return false;
    }

    // Extract ?code= from the request line
    let code = first_line
        .split_whitespace()
        .nth(1)
        .and_then(|path| path.split('?').nth(1))
        .and_then(|query| {
            query
                .split('&')
                .find(|p| p.starts_with("code="))
                .map(|p| p[5..].to_string())
        });

    let Some(code) = code else {
        let resp = http_response(400, "Bad Request", r#"{"error":"missing_code"}"#);
        let _ = stream.write_all(resp.as_bytes()).await;
        return false;
    };

    match pm.validate(&code) {
        Ok(pairing_response) => {
            let json = serde_json::to_string(&pairing_response).unwrap_or_default();
            let resp = http_response(200, "OK", &json);
            let _ = stream.write_all(resp.as_bytes()).await;
            info!("✅ Offline pairing successful");
            true
        }
        Err(PairingError::RateLimited) => {
            let resp = http_response(429, "Too Many Requests", r#"{"error":"rate_limited"}"#);
            let _ = stream.write_all(resp.as_bytes()).await;
            false
        }
        Err(_) => {
            let resp = http_response(401, "Unauthorized", r#"{"error":"invalid_code"}"#);
            let _ = stream.write_all(resp.as_bytes()).await;
            false
        }
    }
}

fn http_response(status: u16, text: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status, text, body.len(), body
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Set custom config directory if provided (both old and new config systems)
    if let Some(ref dir) = cli.config_dir {
        config::set_config_dir(dir.clone());
        common_config::set_config_dir(dir.clone());
    }

    // Determine log level based on command and flags
    let log_level = match &cli.command {
        Commands::Start { verbose, .. } => {
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

    match cli.command {
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
            println!("\n🚀 Start the bridge with: bridge start --agent-command \"gemini --experimental-acp\"");
        }

        Commands::Start { agent_command, bind, qr, verbose: _ } => {
            info!("🌉 Starting ACP Bridge...");

            // Load (or initialise) the common config
            let mut config = CommonConfig::load()?;
            config.ensure_agent_id();
            config.ensure_auth_token();
            config.save()?;

            let enabled: Vec<(String, TransportConfig)> = config
                .enabled_transports()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect();

            if enabled.is_empty() {
                anyhow::bail!(
                    "No transports are enabled in common.toml.\n\
                     Add at least one transport section with `enabled = true`, e.g.:\n\
                     \n\
                     [transports.local]\n\
                     enabled = true\n\
                     port = 8765\n\
                     tls = true"
                );
            }

            // When more than one transport is enabled, ask the user to pick one.
            let (transport_name, transport_cfg) = if enabled.len() == 1 {
                enabled.into_iter().next().unwrap()
            } else {
                println!("\nMultiple transports are enabled. Select one to start:");
                for (i, (name, _)) in enabled.iter().enumerate() {
                    println!("  [{}] {}", i + 1, name);
                }
                print!("Enter number [1]: ");
                use std::io::Write as _;
                std::io::stdout().flush()?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                let choice: usize = input.trim().parse().unwrap_or(1);
                let idx = choice.saturating_sub(1).min(enabled.len() - 1);
                enabled.into_iter().nth(idx).unwrap()
            };

            let config_dir = CommonConfig::config_dir();

            // tailscale-serve defaults to 8766 to avoid conflicting with local (8765).
            let default_port: u16 = if transport_name == "tailscale-serve" { 8766 } else { 8765 };
            let port = transport_cfg.port.unwrap_or(default_port);

            // tailscale-serve proxies from Tailscale edge → localhost, so bind
            // to 127.0.0.1 only; all other transports use the user-supplied bind addr.
            let effective_bind = if transport_name == "tailscale-serve" {
                "127.0.0.1".to_string()
            } else {
                bind.clone()
            };

            let (hostname, pm, tls_config, _ts_guard, _cf_runner) =
                build_transport(&transport_name, &transport_cfg, &config, &config_dir)?;

            if qr {
                if transport_name == "cloudflare" {
                    let json = config.to_connection_json(&hostname, &transport_name)?;
                    qr::display_qr_code(&json, &transport_name)?;
                } else {
                    qr::display_qr_code_with_pairing(&hostname, &pm)?;
                }
            }

            info!("📡 Starting WebSocket server on {}:{} (transport: {})", effective_bind, port, transport_name);
            info!("🤖 Agent command: {}", agent_command);

            let mut bridge = StdioBridge::new(agent_command.clone(), port)
                .with_bind_addr(effective_bind)
                .with_auth_token(Some(config.auth_token.clone()))
                .with_pairing(pm);

            if let Some(tls) = tls_config {
                info!("🔒 TLS fingerprint: {}", tls.fingerprint_short());
                bridge = bridge.with_tls(tls);
            }

            // _ts_guard and _cf_runner live until end of this block (bridge lifetime).
            match bridge.start().await {
                Ok(()) => info!("Bridge exited cleanly"),
                Err(e) => error!("Bridge error: {}", e),
            }
        }

        Commands::ShowQr => {
            let mut config = CommonConfig::load()?;
            config.ensure_agent_id();
            config.ensure_auth_token();
            config.save()?;

            // Detect running bridge by attempting a TCP connection (connect is reliable
            // on macOS where SO_REUSEADDR makes bind-based detection fail).
            let local_port = config
                .transports
                .get("local")
                .and_then(|t| t.port)
                .unwrap_or(8765);

            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], local_port));
            let is_running = std::net::TcpStream::connect_timeout(
                &addr,
                std::time::Duration::from_millis(300),
            )
            .is_ok();

            if is_running {
                // Bridge is running — show static QR with connection details
                let config_dir = CommonConfig::config_dir();
                let tls_config = TlsConfig::load_or_generate(&config_dir, &[])?;
                let ip = match local_ip_address::local_ip() {
                    Ok(addr) => addr.to_string(),
                    Err(_) => "127.0.0.1".to_string(),
                };
                let hostname = format!("wss://{}:{}", ip, local_port);

                // Build JSON with agentId and cert fingerprint
                let json = {
                    use serde_json::{Map, Value};
                    let mut map = Map::new();
                    if !config.agent_id.is_empty() {
                        map.insert("agentId".to_string(), Value::String(config.agent_id.clone()));
                    }
                    map.insert("url".to_string(), Value::String(hostname.clone()));
                    map.insert("protocol".to_string(), Value::String("acp".to_string()));
                    map.insert("version".to_string(), Value::String("1.0".to_string()));
                    if !config.auth_token.is_empty() {
                        map.insert("authToken".to_string(), Value::String(config.auth_token.clone()));
                    }
                    map.insert(
                        "certFingerprint".to_string(),
                        Value::String(tls_config.fingerprint.clone()),
                    );
                    serde_json::to_string(&Value::Object(map))
                        .context("Failed to build connection JSON")?
                };
                qr::display_qr_code(&json, "local")?;
            } else {
                // Bridge not running — start offline registration server
                run_offline_registration(&config).await?;
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

            // Also check old BridgeConfig (Cloudflare)
            match BridgeConfig::load() {
                Ok(config) => {
                    println!("\n✅ Legacy Cloudflare config found");
                    println!("Hostname: {}", config.hostname);
                    println!("Tunnel ID: {}", if config.tunnel_id.is_empty() {
                        "⚠️  Not configured".to_string()
                    } else {
                        config.tunnel_id.clone()
                    });

                    match cloudflared_config_path() {
                        Ok(path) => {
                            if path.exists() {
                                println!("cloudflared config: ✅ {}", path.display());
                            } else {
                                println!("cloudflared config: ⚠️  Not found at {}", path.display());
                            }
                        }
                        Err(e) => println!("cloudflared config: ❌ Cannot determine path: {}", e),
                    }
                }
                Err(_) => {
                    println!("\nℹ️  No legacy Cloudflare config (config.json) — using common.toml.");
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
            } else {
                println!("Tailscale: not installed");
            }
        }
    }

    Ok(())
}

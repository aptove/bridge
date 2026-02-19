use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::sync::Arc;
use tracing::{info, error, warn};

mod cloudflare;
mod cloudflared_runner;
mod bridge;
mod config;
mod pairing;
mod qr;
mod rate_limiter;
mod tls;
mod tailscale;
mod agent_pool;
mod push;

use crate::cloudflare::{CloudflareClient, write_credentials_file, write_cloudflared_config, cloudflared_config_path};
use crate::cloudflared_runner::CloudflaredRunner;
use crate::bridge::StdioBridge;
use crate::config::BridgeConfig;
use crate::pairing::PairingManager;
use crate::tls::TlsConfig;
use crate::agent_pool::{AgentPool, PoolConfig};
use crate::push::PushRelayClient;
use crate::tailscale::{is_tailscale_available, get_tailscale_ipv4, get_tailscale_hostname, tailscale_serve_start, TailscaleServeGuard};

#[derive(Clone, Debug, clap::ValueEnum)]
enum TailscaleMode {
    /// Bridge stays on loopback; tailscale serve provides HTTPS (requires MagicDNS + HTTPS)
    Serve,
    /// Bridge binds directly to the Tailscale IP with self-signed TLS + cert pinning
    Ip,
}

#[derive(Parser)]
#[command(name = "bridge")]
#[command(about = "Bridge stdio-based ACP agents to mobile apps via Cloudflare Zero Trust", long_about = None)]
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
    
    /// Start the bridge server
    Start {
        /// Command to run the ACP agent (e.g., "gemini --experimental-acp")
        #[arg(short, long)]
        agent_command: String,
        
        /// Local port to bind the WebSocket server
        #[arg(short, long, default_value = "8080")]
        port: u16,
        
        /// Address to bind the WebSocket server (use 127.0.0.1 for localhost only)
        #[arg(short, long, default_value = "0.0.0.0")]
        bind: String,
        
        /// Show QR code for mobile connection
        #[arg(short = 'Q', long)]
        qr: bool,
        
        /// Run in stdio-proxy mode (no Cloudflare). Exposes local WebSocket for mobile clients.
        #[arg(long)]
        stdio_proxy: bool,
        
        /// Disable authentication (NOT RECOMMENDED - use only for development)
        #[arg(long)]
        no_auth: bool,
        
        /// Disable TLS encryption (NOT RECOMMENDED - use only for development)
        #[arg(long)]
        no_tls: bool,
        
        /// Maximum concurrent connections per IP address (default: 3)
        #[arg(long, default_value = "3")]
        max_connections_per_ip: usize,
        
        /// Maximum connection attempts per minute per IP address (default: 10)
        #[arg(long, default_value = "10")]
        max_attempts_per_minute: usize,
        
        /// Enable verbose logging (shows info level logs)
        #[arg(short, long)]
        verbose: bool,
        
        /// Keep agent processes alive when clients disconnect (enables session persistence)
        #[arg(long)]
        keep_alive: bool,
        
        /// Idle timeout in seconds before killing disconnected agents (default: 1800 = 30 min)
        #[arg(long, default_value = "1800")]
        session_timeout: u64,
        
        /// Maximum number of concurrent agent processes (default: 10)
        #[arg(long, default_value = "10")]
        max_agents: usize,
        
        /// Buffer agent messages while client is disconnected
        #[arg(long)]
        buffer_messages: bool,

        /// Push relay URL for sending push notifications to mobile devices
        /// when the client is disconnected
        #[arg(long, default_value = "https://push.oss.aptov.com")]
        push_relay_url: Option<String>,

        /// Spawn and manage the cloudflared tunnel daemon (requires prior `bridge setup`)
        #[arg(long)]
        cloudflare: bool,

        /// Use Tailscale as the bridge transport. `serve`: HTTPS via tailscale serve (recommended,
        /// requires MagicDNS + HTTPS). `ip`: direct Tailscale IP bind with self-signed TLS.
        #[arg(long, value_name = "MODE")]
        tailscale: Option<TailscaleMode>,
    },
    
    /// Show connection QR code
    ShowQr,
    
    /// Check configuration status
    Status,
}

/// Ensure Cloudflare config exists — load it if valid, auto-rotate token if near expiry,
/// or run interactive first-time setup.
async fn ensure_cloudflare_config(no_auth: bool) -> Result<BridgeConfig> {
    use std::io::{self, BufRead, Write};

    // If a valid config already exists (has tunnel and service token), check token health
    if let Ok(mut cfg) = BridgeConfig::load() {
        if !cfg.tunnel_id.is_empty() && !cfg.client_id.is_empty() && !cfg.client_secret.is_empty() {
            // Ensure credentials file exists and has a valid secret
            if cfg.tunnel_secret.is_empty() {
                warn!("⚠️  Tunnel secret is missing from config — credentials are lost.");
                warn!("   Delete the config and re-run to trigger full re-setup:");
                warn!("   rm {}", BridgeConfig::config_path().display());
                anyhow::bail!("Tunnel secret lost. Delete config and re-run.");
            }
            // Re-write credentials and config files in case they were deleted or corrupted
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
        Ok(lines.next().context("stdin closed")??
            .trim()
            .to_string())
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Set custom config directory if provided
    if let Some(config_dir) = cli.config_dir {
        config::set_config_dir(config_dir);
    }

    // Determine log level based on command and flags
    let log_level = match &cli.command {
        Commands::Start { verbose, .. } => {
            if *verbose {
                "info"
            } else {
                "warn"  // Default to warn - no message content logged
            }
        }
        // For other commands, default to info
        _ => "info"
    };

    // Initialize logging
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
            
            // Step 1: Create or get existing tunnel
            info!("📡 Creating Cloudflare Tunnel: {}", tunnel_name);
            let tunnel = client.create_or_get_tunnel(&tunnel_name).await?;
            info!("✅ Tunnel created: {}", tunnel.id);
            
            // Step 2: Create DNS record
            let hostname = format!("{}.{}", subdomain, domain);
            info!("🌐 Creating DNS record for: {}", hostname);
            client.create_dns_record(&domain, &subdomain, &tunnel.id).await?;
            info!("✅ DNS record created");
            
            // Step 3: Create Access Application
            info!("🔐 Creating Zero Trust Access Application...");
            let app = client.create_access_application(&hostname).await?;
            info!("✅ Access Application created: {}", app.id);
            
            // Step 4: Generate Service Token
            info!("🎫 Generating Service Token...");
            let service_token = client.create_service_token(&hostname).await?;
            info!("✅ Service Token created");
            
            // Step 5: Configure tunnel ingress
            info!("⚙️  Configuring tunnel ingress rules...");
            client.configure_tunnel_ingress(&tunnel.id, &hostname, 8080).await?;
            info!("✅ Tunnel ingress configured");
            
            // Step 6: Write cloudflared credentials file (~/.cloudflared/<tunnel-id>.json)
            info!("📄 Writing cloudflared credentials file...");
            let credentials_path = write_credentials_file(&account_id, &tunnel.id, &tunnel.secret)
                .context("Failed to write cloudflared credentials file")?;
            info!("✅ Credentials file written to: {}", credentials_path.display());
            
            // Step 7: Write cloudflared config.yml (~/.cloudflared/config.yml)
            info!("📄 Writing cloudflared config.yml...");
            let config_yml_path = write_cloudflared_config(&tunnel.id, &credentials_path, &hostname, 8080)
                .context("Failed to write cloudflared config.yml")?;
            info!("✅ Config file written to: {}", config_yml_path.display());
            
            // Step 8: Save bridge configuration
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
            
            // Generate auth token for secure connections
            config.ensure_auth_token();
            config.stamp_service_token_issued();
            
            config.save()?;
            info!("✅ Configuration saved to: {}", BridgeConfig::config_path().display());
            
            // Display QR code
            println!("\n🎉 Setup complete!\n");
            qr::display_qr_code(&config)?;
            
            println!("\n📱 Scan this QR code with your mobile app to connect.");
            println!("\n⚠️  Important: Keep your configuration file secure. It contains sensitive credentials.");
            println!("\n🚀 Start the bridge with: bridge start --agent-command \"gemini --experimental-acp\" --cloudflare");
        }
        
        Commands::Start { agent_command, port, bind, qr, stdio_proxy, no_auth, no_tls, max_connections_per_ip, max_attempts_per_minute, verbose, keep_alive, session_timeout, max_agents, buffer_messages, push_relay_url, cloudflare, tailscale } => {
            info!("🌉 Starting ACP Bridge...");
            
            if no_auth {
                warn!("⚠️  Authentication disabled with --no-auth flag. This is NOT recommended for production!");
            }
            
            if no_tls {
                warn!("⚠️  TLS disabled with --no-tls flag. Connections will not be encrypted!");
            }
            
            // If stdio_proxy is enabled, bypass Cloudflare and construct a local connection URL.
            // Determine the config directory for TLS certs
            let config_dir = BridgeConfig::config_dir();
            std::fs::create_dir_all(&config_dir)?;

            if stdio_proxy && tailscale.is_some() {
                anyhow::bail!("--tailscale and --stdio-proxy are mutually exclusive. Use one or the other.");
            }

            // Collect extra SANs for TLS cert (populated in tailscale ip mode)
            let mut extra_sans: Vec<String> = vec![];

            // For tailscale ip mode, determine extra SANs BEFORE TLS cert generation
            if let Some(TailscaleMode::Ip) = &tailscale {
                let ts_ip = get_tailscale_ipv4()?;
                extra_sans.push(ts_ip);
                if let Ok(Some(ts_hostname)) = get_tailscale_hostname() {
                    extra_sans.push(ts_hostname);
                }
            }

            // Load or generate TLS config (unless --no-tls, or serve mode where tailscale provides TLS)
            // Cloudflare terminates TLS at its edge; the bridge must serve plain HTTP
            let tls_config = if no_tls || cloudflare || matches!(tailscale, Some(TailscaleMode::Serve)) {
                None
            } else {
                Some(TlsConfig::load_or_generate(&config_dir, &extra_sans)?)
            };

            let config = if stdio_proxy {
                // Determine a sensible local IP to advertise to mobile clients
                // Use local-ip-address crate to avoid external network connections
                let ip = if bind == "0.0.0.0" {
                    match local_ip_address::local_ip() {
                        Ok(addr) => addr.to_string(),
                        Err(_) => "127.0.0.1".to_string(),
                    }
                } else {
                    bind.clone()
                };

                let protocol = if tls_config.is_some() { "wss" } else { "ws" };
                let hostname = format!("{}://{}:{}", protocol, ip, port);

                // Try to load existing config to preserve auth_token, or create new
                let mut cfg = if let Ok(mut existing) = BridgeConfig::load() {
                    // Update dynamic fields but preserve auth_token
                    existing.hostname = hostname;
                    existing.cert_fingerprint = tls_config.as_ref().map(|t| t.fingerprint.clone());
                    existing
                } else {
                    BridgeConfig {
                        hostname,
                        tunnel_id: String::new(),
                        tunnel_secret: String::new(),
                        account_id: String::new(),
                        client_id: String::new(),
                        client_secret: String::new(),
                        domain: String::new(),
                        subdomain: String::new(),
                        auth_token: String::new(),
                        cert_fingerprint: tls_config.as_ref().map(|t| t.fingerprint.clone()),
                        service_token_issued_at: None,
                        api_token: String::new(),
                    }
                };
                
                // Generate auth token if needed and persist
                if !no_auth && cfg.auth_token.is_empty() {
                    cfg.ensure_auth_token();
                    info!("🔑 Generated new auth token");
                    if verbose {
                        println!("🔐 Relay Token (for Bruno/testing): {}", cfg.auth_token);
                    }
                } else if verbose && !no_auth {
                    println!("🔐 Relay Token (for Bruno/testing): {}", cfg.auth_token);
                }
                
                // Always save config for stdio-proxy mode to persist auth_token
                cfg.save()?;
                
                cfg
            } else if let Some(TailscaleMode::Ip) | Some(TailscaleMode::Serve) = &tailscale {
                // Tailscale transport mode
                let (hostname, cert_fingerprint) = match &tailscale {
                    Some(TailscaleMode::Ip) => {
                        // extra_sans was populated above; first entry is the IP
                        let ts_ip = extra_sans.first().cloned().unwrap_or_default();
                        // Prefer MagicDNS hostname for the pairing URL if available
                        let addr = extra_sans.get(1).cloned().unwrap_or(ts_ip);
                        let protocol = if tls_config.is_some() { "wss" } else { "ws" };
                        let h = format!("{}://{}:{}", protocol, addr, port);
                        println!("📡 Tailscale (ip): {}", h);
                        let fp = tls_config.as_ref().map(|t| t.fingerprint.clone());
                        (h, fp)
                    }
                    Some(TailscaleMode::Serve) => {
                        let ts_hostname = get_tailscale_hostname()?
                            .ok_or_else(|| anyhow::anyhow!(
                                "tailscale serve mode requires MagicDNS + HTTPS to be enabled on your tailnet.\n\
                                 Enable HTTPS in the Tailscale admin console: https://tailscale.com/kb/1153/enabling-https"
                            ))?;
                        let h = format!("wss://{}", ts_hostname);
                        // No fingerprint for serve mode — tailscale provides the cert
                        (h, None)
                    }
                    _ => unreachable!(),
                };

                let mut cfg = if let Ok(mut existing) = BridgeConfig::load() {
                    existing.hostname = hostname;
                    existing.cert_fingerprint = cert_fingerprint;
                    existing
                } else {
                    BridgeConfig {
                        hostname,
                        tunnel_id: String::new(),
                        tunnel_secret: String::new(),
                        account_id: String::new(),
                        client_id: String::new(),
                        client_secret: String::new(),
                        domain: String::new(),
                        subdomain: String::new(),
                        auth_token: String::new(),
                        cert_fingerprint,
                        service_token_issued_at: None,
                        api_token: String::new(),
                    }
                };

                if !no_auth && cfg.auth_token.is_empty() {
                    cfg.ensure_auth_token();
                    info!("🔑 Generated new auth token");
                    if verbose {
                        println!("🔐 Relay Token (for Bruno/testing): {}", cfg.auth_token);
                    }
                } else if verbose && !no_auth {
                    println!("🔐 Relay Token (for Bruno/testing): {}", cfg.auth_token);
                }

                cfg.save()?;
                cfg
            } else {
                if cloudflare {
                    // Cloudflare mode: load existing config or run interactive setup
                    ensure_cloudflare_config(no_auth).await?
                } else {
                    let mut cfg = BridgeConfig::load()?;
                    // Ensure auth token exists for loaded config
                    if !no_auth && cfg.auth_token.is_empty() {
                        cfg.ensure_auth_token();
                        cfg.save()?;
                        info!("🔑 Generated new auth token and saved to config");
                        if verbose {
                            println!("🔐 Relay Token (for Bruno/testing): {}", cfg.auth_token);
                        }
                    } else if verbose && !no_auth {
                        println!("🔐 Relay Token (for Bruno/testing): {}", cfg.auth_token);
                    }
                    cfg
                }
            };

            // For Cloudflare mode: embed credentials directly in the QR (JSON format).
            // The pairing-URL handshake can't be used because the pairing endpoint is
            // protected by Cloudflare Access — the app doesn't yet have the tokens needed
            // to reach it. The JSON QR is scanned offline with no network round-trip.
            // For local/TLS mode: use the pairing-URL flow so the fingerprint can be
            // validated before credentials are handed over.

            // Pairing manager for local/TLS mode (QR with one-time code + cert pinning)
            let pairing_manager = if qr && !cloudflare {
                let (client_id, client_secret) = (None, None);
                Some(PairingManager::new_with_cf(
                    config.hostname.clone(),
                    config.auth_token.clone(),
                    config.cert_fingerprint.clone(),
                    client_id,
                    client_secret,
                ))
            } else {
                None
            };

            // Display QR code with pairing URL if enabled (local/TLS mode)
            if let Some(ref pm) = pairing_manager {
                qr::display_qr_code_with_pairing(&config, pm)?;
            }

            info!("📡 Starting WebSocket server on {}:{}", bind, port);
            info!("🤖 Agent command: {}", agent_command);

            let auth_token = if no_auth { None } else { Some(config.auth_token.clone()) };
            
            let mut bridge = StdioBridge::new(agent_command, port)
                .with_bind_addr(bind)
                .with_auth_token(auth_token)
                .with_rate_limits(max_connections_per_ip, max_attempts_per_minute);
            
            // Add pairing if enabled
            if let Some(pm) = pairing_manager {
                bridge = bridge.with_pairing(pm);
            }
            
            // Add TLS if enabled
            if let Some(tls) = tls_config {
                info!("🔒 TLS fingerprint: {}", tls.fingerprint_short());
                bridge = bridge.with_tls(tls);
            }
            
            // Set up push relay client if configured
            let push_client = if let Some(ref relay_url) = push_relay_url {
                let client = PushRelayClient::new(
                    relay_url.clone(),
                    config.auth_token.clone(),
                );
                info!("🔔 Push notifications enabled via relay: {}", relay_url);
                Some(Arc::new(client))
            } else {
                info!("🔕 Push notifications disabled (no --push-relay-url)");
                None
            };

            // Set up agent pool if keep-alive is enabled
            if keep_alive {
                let pool_config = PoolConfig {
                    idle_timeout: std::time::Duration::from_secs(session_timeout),
                    max_agents,
                    buffer_messages,
                    ..Default::default()
                };
                info!("🔄 Keep-alive enabled: timeout={}s, max_agents={}, buffer={}", 
                    session_timeout, max_agents, buffer_messages);
                
                let mut pool = AgentPool::new(pool_config);
                
                // Wire push relay into agent pool for notification triggers
                if let Some(ref push_relay) = push_client {
                    pool = pool.with_push_relay(Arc::clone(push_relay));
                    info!("🔔 Push notifications wired into agent pool");
                }
                
                let pool = std::sync::Arc::new(tokio::sync::RwLock::new(pool));
                
                // Start the background reaper task
                let reaper_pool = std::sync::Arc::clone(&pool);
                agent_pool::start_reaper(reaper_pool, std::time::Duration::from_secs(60));
                
                bridge = bridge.with_agent_pool(pool);
            }
            
            // Wire push relay into bridge for registration handlers
            if let Some(push_relay) = push_client {
                bridge = bridge.with_push_relay((*push_relay).clone());
            }

            // Spawn and manage cloudflared tunnel daemon if requested
            let _cloudflared_runner = if cloudflare {
                if config.tunnel_id.is_empty() {
                    anyhow::bail!("Cloudflare setup incomplete — tunnel_id missing in config.");
                }
                let config_yml = cloudflared_config_path()?;
                info!("🌐 Starting cloudflared tunnel daemon...");
                let mut runner = CloudflaredRunner::spawn(&config_yml, &config.tunnel_id)?;
                runner.wait_for_ready(std::time::Duration::from_secs(30))?;
                println!("🌐 Cloudflare tunnel active: {}", config.hostname);
                // Show QR AFTER tunnel is ready — by the time the user scans, the bridge
                // will already be listening (bridge.start() binds the socket immediately below)
                qr::display_qr_code(&config)?;
                Some(runner)
            } else {
                None
            };

            #[allow(unused_imports)]
            let _tailscale_serve_guard: Option<TailscaleServeGuard> = if let Some(TailscaleMode::Serve) = &tailscale {
                info!("🌐 Starting tailscale serve...");
                let guard = tailscale_serve_start(port)?;
                let hostname_display = get_tailscale_hostname()?.unwrap_or_default();
                println!("📡 Tailscale (serve): wss://{}", hostname_display);
                Some(guard)
            } else {
                None
            };

            bridge.start().await?;
        }
        
        Commands::ShowQr => {
            let config = BridgeConfig::load()?;
            qr::display_qr_code(&config)?;
        }
        
        Commands::Status => {
            match BridgeConfig::load() {
                Ok(config) => {
                    println!("✅ Configuration found\n");
                    println!("Hostname: {}", config.hostname);
                    println!("Tunnel ID: {}", if config.tunnel_id.is_empty() {
                        "⚠️  Not configured (run 'bridge setup')".to_string()
                    } else {
                        config.tunnel_id.clone()
                    });
                    println!("Domain: {}.{}", config.subdomain, config.domain);
                    println!("Config file: {}", BridgeConfig::config_path().display());

                    // Check cloudflared config file presence
                    match cloudflared_config_path() {
                        Ok(path) => {
                            if path.exists() {
                                println!("cloudflared config: ✅ {}", path.display());
                            } else {
                                println!("cloudflared config: ⚠️  Not found at {} (run 'bridge setup')", path.display());
                            }
                        }
                        Err(e) => {
                            println!("cloudflared config: ❌ Cannot determine path: {}", e);
                        }
                    }

                    // Check Tailscale status
                    if is_tailscale_available() {
                        match get_tailscale_ipv4() {
                            Ok(ip) => {
                                println!("Tailscale IP: {}", ip);
                                match get_tailscale_hostname() {
                                    Ok(Some(hostname)) => println!("Tailscale hostname: {}", hostname),
                                    _ => {}
                                }
                            }
                            Err(_) => println!("Tailscale: not enrolled (run 'tailscale up')"),
                        }
                    } else {
                        println!("Tailscale: not installed");
                    }
                }
                Err(e) => {
                    error!("❌ No configuration found: {}", e);
                    println!("\n💡 Run 'bridge setup' to initialize the bridge.");
                }
            }
        }
    }

    Ok(())
}

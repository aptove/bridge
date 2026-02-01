use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::{info, error, warn};

mod cloudflare;
mod bridge;
mod config;
mod pairing;
mod qr;
mod rate_limiter;
mod tls;

use crate::cloudflare::CloudflareClient;
use crate::bridge::StdioBridge;
use crate::config::BridgeConfig;
use crate::pairing::PairingManager;
use crate::tls::TlsConfig;

#[derive(Parser)]
#[command(name = "bridge")]
#[command(about = "Bridge stdio-based ACP agents to mobile apps via Cloudflare Zero Trust", long_about = None)]
struct Cli {
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
    },
    
    /// Show connection QR code
    ShowQr,
    
    /// Check configuration status
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

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
            
            let client = CloudflareClient::new(api_token, account_id);
            
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
            
            // Step 6: Save configuration
            let mut config = BridgeConfig {
                hostname: format!("https://{}", hostname),
                tunnel_id: tunnel.id.clone(),
                tunnel_secret: tunnel.secret.clone(),
                client_id: service_token.client_id,
                client_secret: service_token.client_secret,
                domain,
                subdomain,
                auth_token: String::new(),
                cert_fingerprint: None,
            };
            
            // Generate auth token for secure connections
            config.ensure_auth_token();
            
            config.save()?;
            info!("✅ Configuration saved to: {}", BridgeConfig::config_path().display());
            
            // Display QR code
            println!("\n🎉 Setup complete!\n");
            qr::display_qr_code(&config)?;
            
            println!("\n📱 Scan this QR code with your mobile app to connect.");
            println!("\n⚠️  Important: Keep your configuration file secure. It contains sensitive credentials.");
            println!("\n🚀 Start the bridge with: bridge start --agent-command \"gemini --experimental-acp\"");
        }
        
        Commands::Start { agent_command, port, bind, qr, stdio_proxy, no_auth, no_tls, max_connections_per_ip, max_attempts_per_minute, verbose: _ } => {
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
            
            // Load or generate TLS config (unless --no-tls)
            let tls_config = if no_tls {
                None
            } else {
                Some(TlsConfig::load_or_generate(&config_dir)?)
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

                let mut cfg = BridgeConfig {
                    hostname,
                    tunnel_id: String::new(),
                    tunnel_secret: String::new(),
                    client_id: String::new(),
                    client_secret: String::new(),
                    domain: String::new(),
                    subdomain: String::new(),
                    auth_token: String::new(),
                    cert_fingerprint: tls_config.as_ref().map(|t| t.fingerprint.clone()),
                };
                
                // Generate and persist auth token for stdio-proxy mode
                if !no_auth {
                    cfg.ensure_auth_token();
                }
                
                cfg
            } else {
                let mut cfg = BridgeConfig::load()?;
                // Ensure auth token exists for loaded config
                if !no_auth && cfg.auth_token.is_empty() {
                    cfg.ensure_auth_token();
                    cfg.save()?;
                    info!("🔑 Generated new auth token and saved to config");
                }
                cfg
            };

            // Create pairing manager if QR/pairing is enabled (create once, use for both display and bridge)
            let pairing_manager = if qr {
                Some(PairingManager::new(
                    config.hostname.clone(),
                    config.auth_token.clone(),
                    config.cert_fingerprint.clone(),
                ))
            } else {
                None
            };

            // Display QR code with pairing URL if enabled
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
                    println!("Tunnel ID: {}", config.tunnel_id);
                    println!("Domain: {}.{}", config.subdomain, config.domain);
                    println!("Config file: {}", BridgeConfig::config_path().display());
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

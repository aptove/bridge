use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::{info, error};

mod cloudflare;
mod bridge;
mod config;
mod qr;

use crate::cloudflare::CloudflareClient;
use crate::bridge::StdioBridge;
use crate::config::BridgeConfig;

#[derive(Parser)]
#[command(name = "acp-bridge")]
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
        #[arg(short, long, default_value = "mobile-acp-bridge")]
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
        
        /// Show QR code for mobile connection
        #[arg(short, long)]
        qr: bool,
        
        /// Run in stdio-proxy mode (no Cloudflare). Exposes local WebSocket for mobile clients.
        #[arg(long)]
        stdio_proxy: bool,
        
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
                "debug"
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
            let config = BridgeConfig {
                hostname: format!("https://{}", hostname),
                tunnel_id: tunnel.id.clone(),
                tunnel_secret: tunnel.secret.clone(),
                client_id: service_token.client_id,
                client_secret: service_token.client_secret,
                domain,
                subdomain,
            };
            
            config.save()?;
            info!("✅ Configuration saved to: {}", BridgeConfig::config_path().display());
            
            // Display QR code
            println!("\n🎉 Setup complete!\n");
            qr::display_qr_code(&config)?;
            
            println!("\n📱 Scan this QR code with your mobile app to connect.");
            println!("\n⚠️  Important: Keep your configuration file secure. It contains sensitive credentials.");
            println!("\n🚀 Start the bridge with: acp-bridge start --agent-command \"gemini --experimental-acp\"");
        }
        
        Commands::Start { agent_command, port, qr, stdio_proxy, verbose: _ } => {
            info!("🌉 Starting ACP Bridge...");
            // If stdio_proxy is enabled, bypass Cloudflare and construct a local connection URL.
            let config = if stdio_proxy {
                // Determine a sensible local IP to advertise to mobile clients
                // Try to detect local outbound IP; fall back to 127.0.0.1 on failure
                let ip = {
                    use std::net::UdpSocket;
                    match UdpSocket::bind("0.0.0.0:0") {
                        Ok(sock) => {
                            if sock.connect("8.8.8.8:80").is_ok() {
                                match sock.local_addr() {
                                    Ok(addr) => addr.ip().to_string(),
                                    Err(_) => "127.0.0.1".to_string(),
                                }
                            } else {
                                "127.0.0.1".to_string()
                            }
                        }
                        Err(_) => "127.0.0.1".to_string(),
                    }
                };

                let hostname = format!("ws://{}:{}", ip, port);

                BridgeConfig {
                    hostname,
                    tunnel_id: String::new(),
                    tunnel_secret: String::new(),
                    client_id: String::new(),
                    client_secret: String::new(),
                    domain: String::new(),
                    subdomain: String::new(),
                }
            } else {
                BridgeConfig::load()?
            };

            if qr {
                qr::display_qr_code(&config)?;
            }

            info!("📡 Starting WebSocket server on port {}", port);
            info!("🤖 Agent command: {}", agent_command);

            let bridge = StdioBridge::new(agent_command, port);
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
                    println!("\n💡 Run 'acp-bridge setup' to initialize the bridge.");
                }
            }
        }
    }

    Ok(())
}

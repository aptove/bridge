use std::sync::{Arc, atomic::AtomicU8};

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio::sync::mpsc;
use tracing_subscriber::prelude::*;

use bridge_core::common_config::{self as common_config, CommonConfig};
use bridge_core::config;
use bridge_core::transport;
use bridge_core::tui::{
    app::App,
    events::AppEvent,
    log_layer::{TuiLogLayer, level_name_to_u8},
};

mod plugins;

#[derive(Parser)]
#[command(name = "bridge", version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Bridge stdio-based ACP agents to mobile apps", long_about = None)]
#[command(subcommand_required = false, disable_version_flag = true)]
struct Cli {
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
    version: (),

    #[arg(short = 'c', long, global = true)]
    config_dir: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the setup wizard (same as launching without arguments).
    Setup,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(ref dir) = cli.config_dir {
        config::set_config_dir(dir.clone());
        common_config::set_config_dir(dir.clone());
    }

    // Register OSS transports: Local and Tailscale.
    transport::init(plugins::oss_transports());

    match cli.command {
        Some(Commands::Setup) | None => run_tui().await,
    }
}

async fn run_tui() -> Result<()> {
    let mut config = CommonConfig::load()?;
    config.ensure_agent_id();
    config.ensure_auth_token();
    config.save()?;

    let (event_tx, event_rx) = mpsc::channel::<AppEvent>(512);
    let log_level_arc = Arc::new(AtomicU8::new(level_name_to_u8(&config.log_level)));

    let log_layer = TuiLogLayer::new(event_tx.clone(), Arc::clone(&log_level_arc));
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("trace"))
        .with(log_layer)
        .init();

    let tick_tx = event_tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
        loop {
            interval.tick().await;
            if tick_tx.send(AppEvent::Tick).await.is_err() { break; }
        }
    });

    let key_tx = event_tx.clone();
    std::thread::spawn(move || loop {
        match crossterm::event::read() {
            Ok(crossterm::event::Event::Key(key)) => {
                if key_tx.blocking_send(AppEvent::Key(key)).is_err() { break; }
            }
            Ok(crossterm::event::Event::Mouse(mouse)) => {
                let _ = key_tx.blocking_send(AppEvent::Mouse(mouse));
            }
            Ok(crossterm::event::Event::Resize(w, h)) => {
                let _ = key_tx.blocking_send(AppEvent::Resize(w, h));
            }
            _ => {}
        }
    });

    let app = App::new(config, event_tx, log_level_arc);
    app.run(event_rx).await
}

use std::sync::{Arc, atomic::AtomicU8};

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio::sync::mpsc;
use tracing_subscriber::prelude::*;

use bridge::common_config::{self as common_config, CommonConfig};
use bridge::config;
use bridge::tui::{
    app::App,
    events::AppEvent,
    log_layer::{TuiLogLayer, level_name_to_u8},
};

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
    /// Set up Cloudflare Zero Trust (interactive TUI wizard, no flags required)
    Setup,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Apply custom config directory before anything else.
    if let Some(ref dir) = cli.config_dir {
        config::set_config_dir(dir.clone());
        common_config::set_config_dir(dir.clone());
    }

    match cli.command {
        Some(Commands::Setup) => run_setup_wizard().await,
        None => run_tui().await,
    }
}

/// Launch the full TUI (wizard if needed, then running screen).
async fn run_tui() -> Result<()> {
    // Load config early so we can read the saved log level.
    let mut config = CommonConfig::load()?;
    config.ensure_agent_id();
    config.ensure_auth_token();
    config.save()?;

    // Channel capacity: generous to avoid dropping log records.
    let (event_tx, event_rx) = mpsc::channel::<AppEvent>(512);

    // Shared atomic for runtime log-level changes (App ↔ TuiLogLayer).
    let log_level_arc = Arc::new(AtomicU8::new(level_name_to_u8(&config.log_level)));

    // Install tracing subscriber: TuiLogLayer captures records for the TUI.
    // EnvFilter is "trace" so all events reach the layer; the layer filters by min_level.
    // No fmt layer — stdout would corrupt the ratatui alternate screen.
    let log_layer = TuiLogLayer::new(event_tx.clone(), Arc::clone(&log_level_arc));
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("trace"))
        .with(log_layer)
        .init();

    // Tick timer — keeps the draw loop alive even when no events arrive.
    let tick_tx = event_tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
        loop {
            interval.tick().await;
            if tick_tx.send(AppEvent::Tick).await.is_err() {
                break;
            }
        }
    });

    // Keyboard input thread — crossterm::event::read() blocks.
    let key_tx = event_tx.clone();
    std::thread::spawn(move || loop {
        match crossterm::event::read() {
            Ok(crossterm::event::Event::Key(key)) => {
                if key_tx.blocking_send(AppEvent::Key(key)).is_err() {
                    break;
                }
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

/// Run the `bridge setup` Cloudflare wizard as a standalone TUI flow.
///
/// This simply launches the TUI in a mode where the wizard starts at the
/// Cloudflare setup step (no agent or transport needed yet).
async fn run_setup_wizard() -> Result<()> {
    let (event_tx, event_rx) = mpsc::channel::<AppEvent>(512);

    let log_level_arc = Arc::new(AtomicU8::new(2)); // WARN
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
            if tick_tx.send(AppEvent::Tick).await.is_err() {
                break;
            }
        }
    });

    let key_tx = event_tx.clone();
    std::thread::spawn(move || loop {
        match crossterm::event::read() {
            Ok(crossterm::event::Event::Key(key)) => {
                if key_tx.blocking_send(AppEvent::Key(key)).is_err() {
                    break;
                }
            }
            _ => {}
        }
    });

    // Load existing config (or fresh default) then force Cloudflare setup wizard.
    let mut config = CommonConfig::load()?;
    config.ensure_agent_id();
    config.ensure_auth_token();
    config.save()?;

    // Remove any existing cloudflare transport so the wizard re-runs it.
    config.transports.remove("cloudflare");

    let app = App::new(config, event_tx, log_level_arc);
    app.run(event_rx).await
}

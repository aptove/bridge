//! Bridge orchestration — starts all transports and the WebSocket server.

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::bridge::StdioBridge;
use crate::common_config::{CommonConfig, SlashCommandConfig};
use crate::push::PushRelayClient;
use crate::transport;
use crate::tui::events::{AppEvent, BridgeEvent};
use crate::agent_pool::{AgentPool, PoolConfig, start_reaper};

/// Build the transport handle for a given transport name using the plugin registry.
pub fn build_transport(
    transport_name: &str,
    config: &CommonConfig,
    config_dir: &std::path::PathBuf,
    advertise_addr: Option<&str>,
    cwd: &str,
) -> Result<transport::TransportHandle> {
    let transport_cfg = config.transports.get(transport_name)
        .ok_or_else(|| anyhow::anyhow!("Transport '{}' not found in config", transport_name))?;

    let plugin = transport::get(transport_name)
        .ok_or_else(|| anyhow::anyhow!(
            "Transport '{}' is not supported by this build.", transport_name
        ))?;

    plugin.build(transport_cfg, config, config_dir, advertise_addr, cwd)
}

/// Start the bridge on the given `transport_name`.
///
/// Runs until the bridge exits or `shutdown_rx` fires.
pub async fn run_bridge(
    config: CommonConfig,
    transport_name: String,
    event_tx: mpsc::Sender<AppEvent>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) -> Result<()> {
    let agent_command = config.agent_command.clone()
        .ok_or_else(|| anyhow::anyhow!("No agent_command in config"))?;

    // Acquire exclusive lock on the config dir.
    let _bridge_lock = {
        use fs2::FileExt;
        let lock_path = CommonConfig::config_dir().join("bridge.lock");
        let lock_file = std::fs::OpenOptions::new()
            .create(true).write(true)
            .open(&lock_path)
            .with_context(|| format!("Failed to open bridge lock file: {}", lock_path.display()))?;
        lock_file.try_lock_exclusive().map_err(|_| anyhow::anyhow!(
            "Another bridge instance is already running from this folder."
        ))?;
        lock_file
    };

    let config_dir = CommonConfig::config_dir();
    let cwd = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .to_string_lossy()
        .to_string();

    let transport_cfg = config.transports.get(&transport_name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Transport '{}' not found in config", transport_name))?;

    let bind_address = if transport_name == "tailscale-serve" {
        "127.0.0.1".to_string()
    } else {
        config.bind_address.clone().unwrap_or_else(|| "0.0.0.0".to_string())
    };

    let port = transport_cfg.port.unwrap_or(8765);

    let handle = build_transport(
        &transport_name,
        &config,
        &config_dir,
        config.advertise_addr.as_deref(),
        &cwd,
    )?;

    let hostname = handle.hostname;
    let pm = handle.pairing_manager;
    let tls_config = handle.tls_config;
    let _guard = handle.guard;
    let uses_external_tls = handle.uses_external_tls;

    // Attach push relay URL to pairing responses.
    let pm = if let Some(ref push_cfg) = config.push_relay {
        if !push_cfg.url.is_empty() && !push_cfg.client_id.is_empty() {
            pm.with_relay_url(push_cfg.url.clone())
        } else { pm }
    } else { pm };

    // Send pairing URL to TUI.
    let base_url = hostname.replace("wss://", "https://").replace("ws://", "http://");
    let pairing_url = pm.get_pairing_url(&base_url);
    let _ = event_tx.send(AppEvent::Bridge(BridgeEvent::PairingUrlReady {
        url: pairing_url,
        transport: transport_name.clone(),
    })).await;

    if let Some(tls) = &tls_config {
        let _ = event_tx.send(AppEvent::Bridge(BridgeEvent::TlsFingerprint {
            fingerprint: tls.fingerprint_short(),
        })).await;
    }

    let _ = event_tx.send(AppEvent::Bridge(BridgeEvent::TransportUp {
        name: transport_name.clone(),
        addr: hostname.clone(),
    })).await;

    info!("Bridge started on {} transport: {}", transport_name, hostname);
    info!("Agent command: {}", agent_command);

    // Build push relay client.
    let push_relay_arc: Option<std::sync::Arc<PushRelayClient>> = if let Some(push_cfg) = &config.push_relay {
        if !push_cfg.url.is_empty() && !push_cfg.token_url.is_empty() && !push_cfg.client_id.is_empty() {
            let client = PushRelayClient::new(push_cfg.url.clone(), String::new())
                .with_jwt_credentials(
                    push_cfg.token_url.clone(),
                    push_cfg.client_id.clone(),
                    push_cfg.client_secret.clone(),
                );
            info!("Push relay: JWT auth (client_id={}, relay={})", push_cfg.client_id, push_cfg.url);
            Some(std::sync::Arc::new(client))
        } else {
            warn!("Push relay config incomplete — push notifications disabled");
            None
        }
    } else {
        None
    };

    let mut bridge = StdioBridge::new(agent_command.clone(), port)
        .with_bind_addr(bind_address)
        .with_auth_token(Some(config.auth_token.clone()))
        .with_pairing(pm);

    if let Some(tls) = tls_config {
        bridge = bridge.with_tls(tls);
    } else if uses_external_tls {
        bridge = bridge.with_external_tls();
    }

    let mut pool_builder = AgentPool::new(PoolConfig::default())
        .with_working_dir(cwd.clone().into());
    if let Some(ref relay) = push_relay_arc {
        pool_builder = pool_builder.with_push_relay(std::sync::Arc::clone(relay));
    }
    let pool = std::sync::Arc::new(tokio::sync::RwLock::new(pool_builder));
    let _reaper = start_reaper(pool.clone(), std::time::Duration::from_secs(60));
    bridge = bridge.with_agent_pool(pool);

    if let Some(relay) = push_relay_arc {
        bridge = bridge.with_push_relay(relay);
    }

    let slash_commands = if config.slash_commands.is_empty() {
        vec![
            SlashCommandConfig { name: "help".into(), description: "Show available commands".into(), input_hint: None },
            SlashCommandConfig { name: "clear".into(), description: "Clear conversation history".into(), input_hint: None },
            SlashCommandConfig { name: "compact".into(), description: "Compact conversation history".into(), input_hint: Some("focus topic (optional)".into()) },
            SlashCommandConfig { name: "agent".into(), description: "Configure agent settings".into(), input_hint: None },
        ]
    } else {
        config.slash_commands.clone()
    };
    bridge = bridge.with_slash_commands(slash_commands);

    let memory_path = config_dir.join("MEMORY.md");
    if !memory_path.exists() {
        let _ = std::fs::write(&memory_path, "");
    }
    bridge = bridge.with_memory_path(memory_path);

    let result = tokio::select! {
        r = bridge.start() => r,
        _ = &mut shutdown_rx => {
            info!("Bridge shutdown requested");
            Ok(())
        }
    };

    drop(_bridge_lock);
    let _ = event_tx.send(AppEvent::Bridge(BridgeEvent::BridgeStopped)).await;
    result
}

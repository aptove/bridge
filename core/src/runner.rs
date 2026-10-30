//! Bridge orchestration — starts all transports and the WebSocket server.

use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Scan [preferred, preferred+50] and return the first port that can be bound
/// on `addr`. Returns `preferred` unchanged if none succeed (the real bind will
/// then fail with a clear OS-level error message).
fn find_free_port(addr: &str, preferred: u16) -> u16 {
    for port in preferred..=preferred.saturating_add(50) {
        if std::net::TcpListener::bind(format!("{}:{}", addr, port)).is_ok() {
            return port;
        }
    }
    preferred
}

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
    mut config: CommonConfig,
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

    let mut transport_cfg = config.transports.get(&transport_name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Transport '{}' not found in config", transport_name))?;

    let bind_address = if transport_name == "tailscale-serve" {
        "127.0.0.1".to_string()
    } else {
        config.bind_address.clone().unwrap_or_else(|| "0.0.0.0".to_string())
    };

    // Auto-resolve port conflicts for transports that bind a TCP port directly.
    // If the configured (or default) port is in use, scan upward and persist
    // the chosen port so subsequent restarts are stable without re-pairing.
    if transport_name == "local" || transport_name == "tailscale-serve" {
        let default_port: u16 = if transport_name == "tailscale-serve" { 8766 } else { 8765 };
        let preferred = transport_cfg.port.unwrap_or(default_port);
        let chosen = find_free_port(&bind_address, preferred);
        if chosen != preferred {
            warn!(
                "Port {} is already in use; switching to {} for '{}' transport. Config updated.",
                preferred, chosen, transport_name
            );
            if let Some(tc) = config.transports.get_mut(&transport_name) {
                tc.port = Some(chosen);
            }
            if let Err(e) = config.save() {
                warn!("Could not persist new port to config: {}", e);
            }
            transport_cfg = config.transports.get(&transport_name).cloned()
                .ok_or_else(|| anyhow::anyhow!("Transport config missing after port update"))?;
            let _ = event_tx.send(AppEvent::Bridge(BridgeEvent::PortConflictResolved {
                old_port: preferred,
                new_port: chosen,
            })).await;
        }
    }

    let port = transport_cfg.port.unwrap_or(if transport_name == "tailscale-serve" { 8766 } else { 8765 });

    let handle = build_transport(
        &transport_name,
        &config,
        &config_dir,
        config.advertise_addr.as_deref(),
        &cwd,
    )?;

    let hostname = handle.hostname;
    let pm_owned = handle.pairing_manager;
    let tls_config = handle.tls_config;
    let _guard = handle.guard;
    let uses_external_tls = handle.uses_external_tls;

    // Attach push relay URL to pairing responses.
    let pm_owned = if let Some(ref push_cfg) = config.push_relay {
        if !push_cfg.url.is_empty() && !push_cfg.client_id.is_empty() {
            pm_owned.with_relay_url(push_cfg.url.clone())
        } else { pm_owned }
    } else { pm_owned };

    // Wrap in Arc so the auto-renew task can share ownership with the bridge.
    let pm = Arc::new(pm_owned);

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
        .with_pairing(Arc::clone(&pm));

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
    bridge = bridge.with_memory_path(memory_path)
        .with_memory_max_bytes(config.memory_max_kb * 1024)
        .with_ws_max_message_bytes((config.ws_max_message_mb * 1024 * 1024) as usize);

    // Spawn a task that auto-renews the pairing code every time it expires.
    // The task stops when `cancel_tx` is dropped at the end of this function.
    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
    {
        let pm_renew = Arc::clone(&pm);
        let tx_renew = event_tx.clone();
        let base_url_renew = base_url.clone();
        let transport_renew = transport_name.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = &mut cancel_rx => break,
                }
                // Detect successful pairing — notify TUI and stop renewal.
                if pm_renew.is_used() {
                    let _ = tx_renew.send(AppEvent::Bridge(BridgeEvent::PairingCompleted)).await;
                    break;
                }
                if pm_renew.is_expired() {
                    pm_renew.renew();
                    let _ = tx_renew.send(AppEvent::Bridge(BridgeEvent::PairingUrlReady {
                        url: pm_renew.get_pairing_url(&base_url_renew),
                        transport: transport_renew.clone(),
                    })).await;
                }
            }
        });
    }

    let result = tokio::select! {
        r = bridge.start() => r,
        _ = &mut shutdown_rx => {
            info!("Bridge shutdown requested");
            Ok(())
        }
    };

    // Signal the renewal task to stop.
    let _ = cancel_tx.send(());
    drop(_bridge_lock);
    let _ = event_tx.send(AppEvent::Bridge(BridgeEvent::BridgeStopped)).await;
    result
}

#[cfg(test)]
mod tests {
    use super::find_free_port;

    #[test]
    fn find_free_port_returns_value_in_range() {
        // Pick a high ephemeral port unlikely to be in use.
        let port = find_free_port("127.0.0.1", 19876);
        assert!(
            port >= 19876 && port <= 19876 + 50,
            "Expected port in [19876, 19926], got {}",
            port
        );
    }

    #[test]
    fn find_free_port_skips_occupied_port() {
        // Bind a listener to occupy a specific port, then verify find_free_port skips it.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let occupied = listener.local_addr().unwrap().port();
        let chosen = find_free_port("127.0.0.1", occupied);
        assert_ne!(chosen, occupied, "find_free_port must not return the occupied port");
        // The listener is still held, so chosen must be strictly higher.
        assert!(chosen > occupied, "Expected find_free_port to scan upward from {}", occupied);
    }

    #[test]
    fn find_free_port_returns_preferred_when_free() {
        // Port 0 triggers OS assignment — use a known free approach: bind then release.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let free_port = listener.local_addr().unwrap().port();
        drop(listener); // release the port
        // Now free_port should be available; find_free_port should return it unchanged.
        let chosen = find_free_port("127.0.0.1", free_port);
        assert_eq!(chosen, free_port, "find_free_port should return preferred when it is free");
    }
}

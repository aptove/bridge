//! Bridge orchestration — starts all transports and the WebSocket server.
//!
//! Extracted from `main.rs` so it can be driven by the TUI without the
//! interactive CLI prompts.

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::bridge::StdioBridge;
use crate::cloudflare::{write_credentials_file, write_cloudflared_config_at, cloudflared_config_path};
use crate::cloudflared_runner::CloudflaredRunner;
use crate::common_config::{CommonConfig, SlashCommandConfig, TransportConfig};
use crate::pairing::PairingManager;
use crate::push::PushRelayClient;
use crate::tailscale::{get_tailscale_hostname, tailscale_serve_start, TailscaleServeGuard};
use crate::tls::TlsConfig;
use crate::tui::events::{AppEvent, BridgeEvent};
use crate::agent_pool::{AgentPool, PoolConfig, start_reaper};

/// Build a `PairingManager` and optionally a `TlsConfig` for a single transport.
///
/// Returns `(hostname, pairing_manager, tls_config, tailscale_guard, cf_runner)`.
pub fn build_transport(
    transport_name: &str,
    transport_cfg: &TransportConfig,
    common: &CommonConfig,
    config_dir: &std::path::PathBuf,
    advertise_addr: Option<&str>,
    cwd: &str,
) -> Result<(String, PairingManager, Option<TlsConfig>, Option<TailscaleServeGuard>, Option<CloudflaredRunner>)> {
    let default_port: u16 = if transport_name == "tailscale-serve" { 8766 } else { 8765 };
    let port = transport_cfg.port.unwrap_or(default_port);
    let use_tls = transport_cfg.tls.unwrap_or(true);

    match transport_name {
        "cloudflare" => {
            let hostname = transport_cfg.hostname.clone().unwrap_or_default();
            let pm = PairingManager::new_with_cf(
                common.agent_id.clone(),
                hostname.clone(),
                common.auth_token.clone(),
                None,
                transport_cfg.client_id.clone(),
                transport_cfg.client_secret.clone(),
                cwd.to_string(),
            );

            let tunnel_id = transport_cfg.tunnel_id.clone().unwrap_or_default();
            let runner = if !tunnel_id.is_empty() {
                let per_project_config = config_dir.join("cloudflared.yml");
                let hostname_bare = hostname.trim_start_matches("https://");
                let config_yml = if let (Some(secret), Some(account_id)) = (
                    transport_cfg.tunnel_secret.as_deref(),
                    transport_cfg.account_id.as_deref(),
                ) {
                    let credentials_path = write_credentials_file(account_id, &tunnel_id, secret)
                        .context("Failed to write cloudflared credentials file")?;
                    write_cloudflared_config_at(&tunnel_id, &credentials_path, hostname_bare, port, &per_project_config)
                        .context("Failed to write per-project cloudflared config")?;
                    per_project_config
                } else {
                    warn!("Cloudflare credentials absent; falling back to ~/.cloudflared/config.yml");
                    cloudflared_config_path()?
                };

                let mut runner = CloudflaredRunner::spawn(&config_yml, &tunnel_id)?;
                runner.wait_for_ready(std::time::Duration::from_secs(30))?;
                Some(runner)
            } else {
                warn!("Cloudflare transport: tunnel_id not configured, skipping cloudflared");
                None
            };

            Ok((hostname, pm, None, None, runner))
        }

        "tailscale-serve" => {
            let ts_hostname = get_tailscale_hostname()?
                .ok_or_else(|| anyhow::anyhow!(
                    "tailscale-serve requires MagicDNS + HTTPS enabled on your tailnet"
                ))?;
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
            let guard = tailscale_serve_start(port)?;
            Ok((hostname, pm, None, Some(guard), None))
        }

        _ => {
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

/// Start the bridge on the given `transport_name`.
///
/// This function runs until the bridge exits or `shutdown_rx` fires.
/// Progress / status events are sent via `event_tx`.
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

    let transport_cfg = config.transports.get(&transport_name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Transport '{}' not found in config", transport_name))?;

    let config_dir = CommonConfig::config_dir();
    let cwd = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .to_string_lossy()
        .to_string();

    let bind_address = if transport_name == "tailscale-serve" {
        "127.0.0.1".to_string()
    } else {
        config.bind_address.clone().unwrap_or_else(|| "0.0.0.0".to_string())
    };

    let default_port: u16 = if transport_name == "tailscale-serve" { 8766 } else { 8765 };
    let port = transport_cfg.port.unwrap_or(default_port);

    let (hostname, pm, tls_config, _ts_guard, _cf_runner) = build_transport(
        &transport_name,
        &transport_cfg,
        &config,
        &config_dir,
        config.advertise_addr.as_deref(),
        &cwd,
    )?;

    // Attach push relay URL to pairing responses.
    let pm = if let Some(ref push_cfg) = config.push_relay {
        if !push_cfg.url.is_empty() && !push_cfg.client_id.is_empty() {
            pm.with_relay_url(push_cfg.url.clone())
        } else { pm }
    } else { pm };

    // Send pairing URL to TUI so /qr can render it.
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

    let uses_external_tls = matches!(transport_name.as_str(), "tailscale-serve" | "cloudflare");

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

    // Slash commands.
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

    // MEMORY.md
    let memory_path = config_dir.join("MEMORY.md");
    if !memory_path.exists() {
        let _ = std::fs::write(&memory_path, "");
    }
    bridge = bridge.with_memory_path(memory_path);

    // Run the bridge, racing against the shutdown signal.
    let result = tokio::select! {
        r = bridge.start() => r,
        _ = &mut shutdown_rx => {
            info!("Bridge shutdown requested");
            Ok(())
        }
    };

    // Release the lock BEFORE sending BridgeStopped so that when the TUI
    // starts a new bridge in response to that event, the lock is already free.
    drop(_bridge_lock);

    let _ = event_tx.send(AppEvent::Bridge(BridgeEvent::BridgeStopped)).await;

    result
}

use anyhow::Result;
use bridge_core::{
    common_config::{CommonConfig, TransportConfig},
    pairing::PairingManager,
    tailscale::{get_tailscale_hostname, tailscale_serve_start},
    tls::TlsConfig,
    transport::{TransportHandle, TransportPlugin},
};

pub fn oss_transports() -> Vec<Box<dyn TransportPlugin>> {
    vec![
        Box::new(LocalTransportPlugin),
        Box::new(TailscaleTransportPlugin),
    ]
}

// ── Local ─────────────────────────────────────────────────────────────────────

pub struct LocalTransportPlugin;

impl TransportPlugin for LocalTransportPlugin {
    fn name(&self) -> &'static str { "local" }
    fn display_name(&self) -> &'static str { "Local Bridge Server" }

    fn is_configured(&self, config: &CommonConfig) -> bool {
        config.transports.get("local").map(|t| t.enabled).unwrap_or(false)
    }

    fn build(
        &self,
        cfg: &TransportConfig,
        common: &CommonConfig,
        config_dir: &std::path::PathBuf,
        advertise_addr: Option<&str>,
        cwd: &str,
    ) -> Result<TransportHandle> {
        let port = cfg.port.unwrap_or(8765);
        let use_tls = cfg.tls.unwrap_or(true);
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
            None, None,
            cwd.to_string(),
        );
        Ok(TransportHandle { hostname, pairing_manager: pm, tls_config, guard: None, uses_external_tls: false })
    }
}

// ── Tailscale ─────────────────────────────────────────────────────────────────

pub struct TailscaleTransportPlugin;

impl TransportPlugin for TailscaleTransportPlugin {
    fn name(&self) -> &'static str { "tailscale-serve" }
    fn display_name(&self) -> &'static str { "Tailscale (Recommended)" }

    fn is_configured(&self, config: &CommonConfig) -> bool {
        config.transports.get("tailscale-serve").map(|t| t.enabled).unwrap_or(false)
    }

    fn build(
        &self,
        cfg: &TransportConfig,
        common: &CommonConfig,
        _config_dir: &std::path::PathBuf,
        _advertise_addr: Option<&str>,
        cwd: &str,
    ) -> Result<TransportHandle> {
        let port = cfg.port.unwrap_or(8766);
        let ts_hostname = get_tailscale_hostname()?
            .ok_or_else(|| anyhow::anyhow!(
                "tailscale-serve requires MagicDNS + HTTPS enabled on your tailnet"
            ))?;
        let hostname = format!("wss://{}", ts_hostname);
        let pm = PairingManager::new_with_cf(
            common.agent_id.clone(),
            hostname.clone(),
            common.auth_token.clone(),
            None, None, None,
            cwd.to_string(),
        ).with_tailscale_path();
        let guard = tailscale_serve_start(port)?;
        Ok(TransportHandle {
            hostname,
            pairing_manager: pm,
            tls_config: None,
            guard: Some(Box::new(guard)),
            uses_external_tls: true,
        })
    }
}

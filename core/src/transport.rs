//! Transport plugin system — allows OSS and enterprise builds to register
//! different transport backends without coupling the core to any specific
//! implementation.

use std::any::Any;
use std::pin::Pin;
use std::sync::OnceLock;

use anyhow::Result;

use crate::common_config::{CommonConfig, TransportConfig};
use crate::pairing::PairingManager;
use crate::tls::TlsConfig;

/// The result of building a transport, returned by [`TransportPlugin::build`].
pub struct TransportHandle {
    pub hostname: String,
    pub pairing_manager: PairingManager,
    pub tls_config: Option<TlsConfig>,
    /// Boxed guard — dropped when the bridge exits, which tears down any
    /// auxiliary processes (e.g. `tailscale serve`).
    pub guard: Option<Box<dyn Any + Send>>,
    /// Whether TLS is handled externally (e.g. Tailscale, Cloudflare).
    pub uses_external_tls: bool,
}

/// A descriptor for a single wizard input field.
pub struct WizardField {
    pub label: &'static str,
    pub placeholder: &'static str,
    pub default_value: &'static str,
}

/// A registered transport implementation.
///
/// Implement this trait and call [`init`] from your binary's `main` to
/// register the transport.  The OSS binary registers `local` and
/// `tailscale-serve`; the enterprise binary additionally registers
/// `cloudflare`.
pub trait TransportPlugin: Send + Sync + 'static {
    /// Short internal name (matches the key in `common.toml [transports]`).
    fn name(&self) -> &'static str;
    /// Human-readable label shown in the transport picker.
    fn display_name(&self) -> &'static str;
    /// Whether this transport is already configured and ready to run.
    fn is_configured(&self, config: &CommonConfig) -> bool;
    /// Build the transport, returning a [`TransportHandle`].
    fn build(
        &self,
        cfg: &TransportConfig,
        common: &CommonConfig,
        config_dir: &std::path::PathBuf,
        advertise_addr: Option<&str>,
        cwd: &str,
    ) -> Result<TransportHandle>;

    /// Optional: wizard fields needed to configure this transport for the
    /// first time.  Returns `None` for transports that self-configure (local,
    /// tailscale-serve).
    fn wizard_fields(&self) -> Option<Vec<WizardField>> {
        None
    }

    /// Optional: run async setup using the values entered in the wizard form
    /// and return an updated [`TransportConfig`].
    ///
    /// Only called when [`wizard_fields`] returns `Some(_)`.
    fn run_setup<'a>(
        &'a self,
        _fields: Vec<String>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<TransportConfig, String>> + Send + 'a>>
    {
        Box::pin(async { Err("run_setup not implemented for this transport".to_string()) })
    }
}

// ── Global registry ──────────────────────────────────────────────────────────

static REGISTRY: OnceLock<Vec<Box<dyn TransportPlugin>>> = OnceLock::new();

/// Register all transport plugins.  Call once from `main` before starting
/// the TUI.  Subsequent calls are silently ignored.
pub fn init(plugins: Vec<Box<dyn TransportPlugin>>) {
    REGISTRY.set(plugins).ok();
}

/// All registered transports.
pub fn all() -> &'static [Box<dyn TransportPlugin>] {
    REGISTRY.get().map(|v| v.as_slice()).unwrap_or(&[])
}

/// Look up a transport by name.
pub fn get(name: &str) -> Option<&'static dyn TransportPlugin> {
    all().iter().find(|p| p.name() == name).map(|p| p.as_ref())
}

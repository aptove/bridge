//! CommonConfig — single source of truth for agent identity and transport settings.
//!
//! Stored as `common.toml` in the bridge config directory.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Global custom config directory for CommonConfig (set via --config-dir).
static COMMON_CUSTOM_CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Set a custom config directory (call before any config operations).
pub fn set_config_dir(path: PathBuf) {
    COMMON_CUSTOM_CONFIG_DIR.set(path).ok();
}

/// Stable agent identity and multi-transport settings.
///
/// Replaces the old `BridgeConfig` / `bridge.toml`. Stored as `common.toml`.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CommonConfig {
    /// Stable UUID that identifies this agent across all transports.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent_id: String,

    /// Bearer token required for WebSocket connections.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth_token: String,

    /// Per-transport configuration, keyed by transport name
    /// (e.g., `"local"`, `"cloudflare"`, `"tailscale-serve"`, `"tailscale-ip"`).
    #[serde(default)]
    pub transports: HashMap<String, TransportConfig>,
}

/// Configuration for a single transport.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct TransportConfig {
    /// Whether this transport is active.
    #[serde(default)]
    pub enabled: bool,

    /// TCP port to bind (local / tailscale-ip transports).
    pub port: Option<u16>,

    /// Enable TLS on this transport (default: true for local and tailscale-ip).
    pub tls: Option<bool>,

    // ---- Cloudflare Zero Trust fields (transport name: "cloudflare") ----
    pub hostname: Option<String>,
    pub tunnel_id: Option<String>,
    pub tunnel_secret: Option<String>,
    pub account_id: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub domain: Option<String>,
    pub subdomain: Option<String>,
}

impl Default for CommonConfig {
    fn default() -> Self {
        let mut transports = HashMap::new();
        transports.insert(
            "local".to_string(),
            TransportConfig {
                enabled: true,
                port: Some(8765),
                tls: Some(true),
                ..Default::default()
            },
        );
        Self {
            agent_id: String::new(),
            auth_token: String::new(),
            transports,
        }
    }
}

impl CommonConfig {
    /// Path to the `common.toml` file (default config dir).
    pub fn config_path() -> PathBuf {
        Self::config_dir().join("common.toml")
    }

    /// Config directory (system default or custom override).
    pub fn config_dir() -> PathBuf {
        let dir = if let Some(custom) = COMMON_CUSTOM_CONFIG_DIR.get() {
            custom.clone()
        } else {
            directories::ProjectDirs::from("com", "aptove", "bridge")
                .expect("Failed to determine config directory")
                .config_dir()
                .to_path_buf()
        };
        fs::create_dir_all(&dir).ok();
        dir
    }

    /// Load from `common.toml` at the default location, or return defaults.
    pub fn load() -> Result<Self> {
        Self::load_from_dir(&Self::config_dir())
    }

    /// Load from `common.toml` in a specific directory, or return defaults.
    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        let path = dir.join("common.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {:?}", path))?;
        let config: Self = toml::from_str(&text)
            .with_context(|| format!("Failed to parse {:?}", path))?;
        Ok(config)
    }

    /// Save to `common.toml` with 0600 permissions (default config dir).
    pub fn save(&self) -> Result<()> {
        self.save_to_dir(&Self::config_dir())
    }

    /// Save to `common.toml` in a specific directory with 0600 permissions.
    pub fn save_to_dir(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        let path = dir.join("common.toml");
        let text = toml::to_string_pretty(self).context("Failed to serialize CommonConfig")?;
        fs::write(&path, &text).with_context(|| format!("Failed to write {:?}", path))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&path, perms)?;
        }
        Ok(())
    }

    /// Generate a UUID v4 `agent_id` if one is not already set.
    pub fn ensure_agent_id(&mut self) {
        if self.agent_id.is_empty() {
            self.agent_id = uuid::Uuid::new_v4().to_string();
        }
    }

    /// Generate a random URL-safe authentication token (32 random bytes, base64).
    pub fn generate_auth_token() -> String {
        use base64::{engine::general_purpose, Engine as _};
        let bytes: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
        general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    /// Ensure `auth_token` is populated, generating one if needed.
    pub fn ensure_auth_token(&mut self) {
        if self.auth_token.is_empty() {
            self.auth_token = Self::generate_auth_token();
        }
    }

    /// Returns all enabled transports, sorted by name for deterministic ordering.
    pub fn enabled_transports(&self) -> Vec<(&str, &TransportConfig)> {
        let mut result: Vec<_> = self
            .transports
            .iter()
            .filter(|(_, t)| t.enabled)
            .map(|(k, v)| (k.as_str(), v))
            .collect();
        result.sort_by_key(|(k, _)| *k);
        result
    }

    /// Build a static connection JSON payload for a QR code.
    ///
    /// Includes `agentId`, `url`, `protocol`, `version`, `authToken`, and
    /// Cloudflare credentials if present in the transport config.
    pub fn to_connection_json(&self, hostname: &str, transport_name: &str) -> Result<String> {
        use serde_json::{Map, Value};
        let transport = self.transports.get(transport_name);
        let mut map = Map::new();
        if !self.agent_id.is_empty() {
            map.insert("agentId".to_string(), Value::String(self.agent_id.clone()));
        }
        map.insert("url".to_string(), Value::String(hostname.to_string()));
        map.insert("protocol".to_string(), Value::String("acp".to_string()));
        map.insert("version".to_string(), Value::String("1.0".to_string()));
        if !self.auth_token.is_empty() {
            map.insert(
                "authToken".to_string(),
                Value::String(self.auth_token.clone()),
            );
        }
        if let Some(t) = transport {
            if let Some(ref id) = t.client_id {
                if !id.is_empty() {
                    map.insert("clientId".to_string(), Value::String(id.clone()));
                }
            }
            if let Some(ref secret) = t.client_secret {
                if !secret.is_empty() {
                    map.insert("clientSecret".to_string(), Value::String(secret.clone()));
                }
            }
        }
        serde_json::to_string(&Value::Object(map)).context("Failed to serialize connection info")
    }
}

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

/// A slash command advertised to connected clients via `available_commands_update`.
///
/// Define these in `common.toml` for agents that don't send `available_commands_update`
/// themselves (e.g. Copilot CLI, Goose).
///
/// Example `common.toml` entry:
/// ```toml
/// [[slash_commands]]
/// name        = "fix"
/// description = "Fix the selected code"
///
/// [[slash_commands]]
/// name        = "explain"
/// description = "Explain the code, optionally with a focus"
/// input_hint  = "what to focus on"
/// ```
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SlashCommandConfig {
    /// Command name without the leading `/` (e.g. `"fix"`).
    pub name: String,
    /// Human-readable description shown in the picker.
    pub description: String,
    /// If set, the command accepts free-text input; this string is shown as
    /// the placeholder hint in the text field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hint: Option<String>,
}

/// Push relay configuration for sending background notifications.
///
/// All four fields are required — push is silently disabled if the section is
/// absent or any field is empty.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct PushRelayConfig {
    /// Base URL of the push relay service (e.g. "https://push.aptove.com").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// Base URL of the JWT token service (e.g. "https://token.aptove.com").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token_url: String,
    /// OAuth2 client_id issued by the token service.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_id: String,
    /// OAuth2 client_secret issued by the token service.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_secret: String,
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
    /// (e.g., `"local"`, `"cloudflare"`, `"tailscale-serve"`).
    #[serde(default)]
    pub transports: HashMap<String, TransportConfig>,

    /// Slash commands to advertise to clients via `available_commands_update`.
    /// Used for agents that don't send this notification themselves.
    /// The bridge injects the notification after every session/new or session/load.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slash_commands: Vec<SlashCommandConfig>,

    /// Push relay configuration. Push is disabled when this section is absent
    /// or any required field is empty — no hardcoded defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_relay: Option<PushRelayConfig>,

    /// Agent command to launch (e.g., "copilot --acp"). Stored here so the
    /// wizard only asks once; previously it was a CLI flag on `bridge run`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_command: Option<String>,

    /// TCP address to bind the WebSocket server (default: "0.0.0.0").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_address: Option<String>,

    /// Override the advertised LAN address in the QR / pairing URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertise_addr: Option<String>,

    /// Prevent system sleep while the bridge is running (default: true).
    #[serde(default = "keep_alive_default")]
    pub keep_alive: bool,

    /// Minimum log level shown in the TUI (ERROR / WARN / INFO / DEBUG / TRACE).
    #[serde(default = "log_level_default")]
    pub log_level: String,
}

fn keep_alive_default() -> bool { true }
fn log_level_default() -> String { "WARN".to_string() }

/// Configuration for a single transport.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct TransportConfig {
    /// Whether this transport is active.
    #[serde(default)]
    pub enabled: bool,

    /// TCP port to bind (local transport).
    pub port: Option<u16>,

    /// Enable TLS on this transport (default: true for local).
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
        // No transports pre-enabled: the setup wizard will ask the user to
        // choose one on first run (or any time no transport is configured).
        Self {
            agent_id: String::new(),
            auth_token: String::new(),
            transports: HashMap::new(),
            slash_commands: Vec::new(),
            push_relay: None,
            agent_command: None,
            bind_address: None,
            advertise_addr: None,
            keep_alive: true,
            log_level: "WARN".to_string(),
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
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".aptove-bridge")
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
    pub fn to_connection_json(&self, hostname: &str, transport_name: &str, cwd: &str) -> Result<String> {
        use serde_json::{Map, Value};
        let transport = self.transports.get(transport_name);
        let mut map = Map::new();
        if !self.agent_id.is_empty() {
            map.insert("agentId".to_string(), Value::String(self.agent_id.clone()));
        }
        map.insert("url".to_string(), Value::String(hostname.to_string()));
        map.insert("protocol".to_string(), Value::String("acp".to_string()));
        map.insert("version".to_string(), Value::String("1.0".to_string()));
        map.insert("cwd".to_string(), Value::String(cwd.to_string()));
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

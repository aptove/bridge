use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Global custom config directory (set via --config-dir)
static CUSTOM_CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Set a custom config directory (call before any config operations)
pub fn set_config_dir(path: PathBuf) {
    CUSTOM_CONFIG_DIR.set(path).ok();
}

/// Configuration for the ACP-Cloudflare bridge
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BridgeConfig {
    pub hostname: String,
    pub tunnel_id: String,
    pub tunnel_secret: String,
    /// Cloudflare account ID (needed to write cloudflared credentials file)
    #[serde(default)]
    pub account_id: String,
    pub client_id: String,
    pub client_secret: String,
    pub domain: String,
    pub subdomain: String,
    /// Authentication token for WebSocket connections (generated on first run)
    #[serde(default)]
    pub auth_token: String,
    /// TLS certificate fingerprint (SHA256, hex encoded with colons)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_fingerprint: Option<String>,
    /// Unix timestamp (seconds) when the Cloudflare service token was last issued.
    /// Used to detect upcoming expiry (token duration: 1 year = 8760h).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_token_issued_at: Option<i64>,
    /// Cloudflare API token — stored so auto-rotation works without re-prompting.
    /// Stored with 0600 permissions alongside other secrets.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_token: String,
}

impl BridgeConfig {
    /// Get the default configuration file path
    pub fn config_path() -> PathBuf {
        Self::config_dir().join("config.json")
    }
    
    /// Get the configuration directory path
    pub fn config_dir() -> PathBuf {
        // Use custom config dir if set, otherwise use system default
        let config_dir_path = if let Some(custom_dir) = CUSTOM_CONFIG_DIR.get() {
            custom_dir.clone()
        } else {
            let config_dir = directories::ProjectDirs::from("com", "aptove", "bridge")
                .expect("Failed to determine config directory");
            config_dir.config_dir().to_path_buf()
        };
        
        fs::create_dir_all(&config_dir_path).ok();
        
        config_dir_path
    }

    /// Save configuration to disk with secure permissions
    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path();
        let json = serde_json::to_string_pretty(self)
            .context("Failed to serialize configuration")?;
        
        fs::write(&config_path, &json)
            .context(format!("Failed to write configuration to {:?}", config_path))?;
        
        // Set restrictive file permissions (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&config_path)?.permissions();
            perms.set_mode(0o600); // rw-------
            fs::set_permissions(&config_path, perms)?;
        }
        
        Ok(())
    }

    /// Generate a random authentication token
    pub fn generate_auth_token() -> String {
        use base64::{engine::general_purpose, Engine as _};
        let random_bytes: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
        general_purpose::URL_SAFE_NO_PAD.encode(random_bytes)
    }

    /// Ensure auth_token is populated, generating one if needed
    pub fn ensure_auth_token(&mut self) {
        if self.auth_token.is_empty() {
            self.auth_token = Self::generate_auth_token();
        }
    }

    /// Service token lifetime: 1 year in seconds
    const SERVICE_TOKEN_LIFETIME_SECS: i64 = 365 * 24 * 3600;
    /// Rotate when fewer than 30 days remain
    const SERVICE_TOKEN_ROTATE_THRESHOLD_SECS: i64 = 30 * 24 * 3600;

    /// Returns true if the service token is expired or will expire within 30 days.
    pub fn service_token_needs_rotation(&self) -> bool {
        let issued_at = match self.service_token_issued_at {
            Some(ts) => ts,
            // No timestamp recorded → assume old/unknown, rotate to be safe
            None => return !self.client_id.is_empty(),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let age = now - issued_at;
        age >= Self::SERVICE_TOKEN_LIFETIME_SECS - Self::SERVICE_TOKEN_ROTATE_THRESHOLD_SECS
    }

    /// Record now as the service token issuance time.
    pub fn stamp_service_token_issued(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.service_token_issued_at = Some(now);
    }

    /// Load configuration from disk
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path();
        let json = fs::read_to_string(&config_path)
            .context(format!("Failed to read configuration from {:?}", config_path))?;
        
        let config: Self = serde_json::from_str(&json)
            .context("Failed to parse configuration file")?;
        
        Ok(config)
    }

    /// Get connection info as JSON for QR code
    pub fn to_connection_json(&self) -> Result<String> {
        use serde_json::{Map, Value};

        let mut map = Map::new();
        map.insert("url".to_string(), Value::String(self.hostname.clone()));
        map.insert("protocol".to_string(), Value::String("acp".to_string()));
        map.insert("version".to_string(), Value::String("1.0".to_string()));

        if !self.client_id.is_empty() {
            map.insert("clientId".to_string(), Value::String(self.client_id.clone()));
        }

        if !self.client_secret.is_empty() {
            map.insert("clientSecret".to_string(), Value::String(self.client_secret.clone()));
        }

        // Include auth token for WebSocket authentication
        if !self.auth_token.is_empty() {
            map.insert("authToken".to_string(), Value::String(self.auth_token.clone()));
        }
        
        // Include TLS certificate fingerprint for pinning
        if let Some(ref fingerprint) = self.cert_fingerprint {
            map.insert("certFingerprint".to_string(), Value::String(fingerprint.clone()));
        }

        serde_json::to_string(&Value::Object(map))
            .context("Failed to serialize connection info")
    }
}

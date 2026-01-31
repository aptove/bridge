use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Configuration for the ACP-Cloudflare bridge
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BridgeConfig {
    pub hostname: String,
    pub tunnel_id: String,
    pub tunnel_secret: String,
    pub client_id: String,
    pub client_secret: String,
    pub domain: String,
    pub subdomain: String,
}

impl BridgeConfig {
    /// Get the default configuration file path
    pub fn config_path() -> PathBuf {
        let config_dir = directories::ProjectDirs::from("com", "bridge", "bridge")
            .expect("Failed to determine config directory");
        
        let config_dir_path = config_dir.config_dir();
        fs::create_dir_all(config_dir_path).ok();
        
        config_dir_path.join("config.json")
    }

    /// Save configuration to disk
    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path();
        let json = serde_json::to_string_pretty(self)
            .context("Failed to serialize configuration")?;
        
        fs::write(&config_path, json)
            .context(format!("Failed to write configuration to {:?}", config_path))?;
        
        Ok(())
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

        serde_json::to_string(&Value::Object(map))
            .context("Failed to serialize connection info")
    }
}

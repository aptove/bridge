use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Push relay client for forwarding device tokens and sending push notifications
/// via the centralized push relay service (Cloudflare Worker).
///
/// The bridge never holds APNs/FCM credentials. It only knows the relay URL
/// and uses its auth_token as the relay_token for isolation.
#[derive(Clone)]
pub struct PushRelayClient {
    relay_url: String,
    relay_token: String,
    http_client: reqwest::Client,
    /// Per-token debounce tracking: token → last notification time
    debounce: Arc<RwLock<HashMap<String, Instant>>>,
    /// Debounce cooldown duration (default 30s)
    cooldown: Duration,
}

/// Request to register a device token with the relay
#[derive(Debug, Serialize)]
struct RegisterRequest {
    relay_token: String,
    device_token: String,
    platform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    bundle_id: Option<String>,
}

/// Request to unregister a device token
#[derive(Debug, Serialize)]
struct UnregisterRequest {
    relay_token: String,
    device_token: String,
}

/// Request to send a push notification
#[derive(Debug, Serialize)]
struct PushRequest {
    relay_token: String,
    title: String,
    body: String,
}

/// Push relay API response
#[derive(Debug, Deserialize)]
struct RelayResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

impl PushRelayClient {
    /// Create a new push relay client.
    ///
    /// - `relay_url`: Base URL of the push relay (e.g., "https://push-relay.example.workers.dev")
    /// - `relay_token`: The bridge's auth_token, used for isolation at the relay
    pub fn new(relay_url: String, relay_token: String) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            relay_url: relay_url.trim_end_matches('/').to_string(),
            relay_token,
            http_client,
            debounce: Arc::new(RwLock::new(HashMap::new())),
            cooldown: Duration::from_secs(30),
        }
    }

    /// Register a device token with the push relay.
    ///
    /// Called when the mobile app sends `bridge/registerPushToken` over WebSocket.
    /// The relay stores the mapping: relay_token → device_token.
    pub async fn register_device(
        &self,
        device_token: &str,
        platform: &str,
        bundle_id: Option<&str>,
    ) -> Result<()> {
        let url = format!("{}/register", self.relay_url);
        let body = RegisterRequest {
            relay_token: self.relay_token.clone(),
            device_token: device_token.to_string(),
            platform: platform.to_string(),
            bundle_id: bundle_id.map(|s| s.to_string()),
        };

        info!("📱 Registering {} device token with push relay", platform);
        debug!("Push relay URL: {}", url);

        let res = self
            .http_client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to contact push relay for registration")?;

        let status = res.status();
        let response: RelayResponse = res
            .json()
            .await
            .context("Failed to parse push relay response")?;

        if response.ok {
            info!("✅ Device token registered with push relay");
            Ok(())
        } else {
            let err_msg = response
                .error
                .or(response.message)
                .unwrap_or_else(|| format!("HTTP {}", status));
            error!("❌ Push relay registration failed: {}", err_msg);
            anyhow::bail!("Push relay registration failed: {}", err_msg)
        }
    }

    /// Unregister a device token from the push relay.
    pub async fn unregister_device(&self, device_token: &str) -> Result<()> {
        let url = format!("{}/register", self.relay_url);
        let body = UnregisterRequest {
            relay_token: self.relay_token.clone(),
            device_token: device_token.to_string(),
        };

        info!("📱 Unregistering device token from push relay");

        let res = self
            .http_client
            .delete(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to contact push relay for unregistration")?;

        let response: RelayResponse = res
            .json()
            .await
            .context("Failed to parse push relay response")?;

        if response.ok {
            info!("✅ Device token unregistered from push relay");
        }
        Ok(())
    }

    /// Send a push notification via the relay.
    ///
    /// Includes per-agent debounce: if a notification was sent within the
    /// cooldown window (default 30s), the new one is silently dropped.
    ///
    /// The notification content is fixed ("Your agent has new activity")
    /// to prevent leaking agent response content.
    pub async fn notify(&self, agent_name: &str) -> Result<bool> {
        // Debounce check
        let debounce_key = self.relay_token.clone();
        {
            let debounce = self.debounce.read().await;
            if let Some(last) = debounce.get(&debounce_key) {
                if last.elapsed() < self.cooldown {
                    debug!(
                        "Push notification throttled ({}s remaining)",
                        (self.cooldown - last.elapsed()).as_secs()
                    );
                    return Ok(false);
                }
            }
        }

        // Update debounce timestamp
        {
            let mut debounce = self.debounce.write().await;
            debounce.insert(debounce_key, Instant::now());
        }

        let url = format!("{}/push", self.relay_url);
        let body = PushRequest {
            relay_token: self.relay_token.clone(),
            title: agent_name.to_string(),
            body: "Your agent has new activity".to_string(),
        };

        info!("🔔 Sending push notification via relay for agent '{}'", agent_name);

        let res = self
            .http_client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to contact push relay for notification")?;

        let status = res.status();
        let response: RelayResponse = res
            .json()
            .await
            .context("Failed to parse push relay response")?;

        if response.ok {
            info!("✅ Push notification sent via relay");
            Ok(true)
        } else {
            let err_msg = response
                .error
                .or(response.message)
                .unwrap_or_else(|| format!("HTTP {}", status));
            warn!("⚠️  Push relay notification failed: {}", err_msg);
            Ok(false)
        }
    }
}

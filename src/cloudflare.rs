use anyhow::{Context, Result};
use reqwest::{Client, header};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

const CLOUDFLARE_API_BASE: &str = "https://api.cloudflare.com/client/v4";

/// Cloudflare API client for Zero Trust operations
pub struct CloudflareClient {
    client: Client,
    api_token: String,
    account_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Tunnel {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub secret: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccessApplication {
    pub id: String,
    pub name: String,
    pub domain: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ServiceToken {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Deserialize)]
struct CloudflareResponse<T> {
    result: T,
    success: bool,
    errors: Vec<CloudflareError>,
    messages: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CloudflareError {
    code: i32,
    message: String,
}

impl CloudflareClient {
    /// Create a new Cloudflare API client
    pub fn new(api_token: String, account_id: String) -> Self {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {}", api_token))
                .expect("Invalid API token format"),
        );
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );

        let client = Client::builder()
            .default_headers(headers)
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            api_token,
            account_id,
        }
    }

    /// Create or retrieve existing tunnel
    pub async fn create_or_get_tunnel(&self, name: &str) -> Result<Tunnel> {
        // First, check if tunnel already exists
        let list_url = format!(
            "{}/accounts/{}/cfd_tunnel",
            CLOUDFLARE_API_BASE, self.account_id
        );

        let response: CloudflareResponse<Vec<Tunnel>> = self
            .client
            .get(&list_url)
            .send()
            .await
            .context("Failed to list tunnels")?
            .json()
            .await
            .context("Failed to parse tunnel list response")?;

        if let Some(existing) = response.result.iter().find(|t| t.name == name) {
            debug!("Found existing tunnel: {}", existing.id);
            return Ok(existing.clone());
        }

        // Create new tunnel
        debug!("Creating new tunnel: {}", name);
        let create_url = format!(
            "{}/accounts/{}/cfd_tunnel",
            CLOUDFLARE_API_BASE, self.account_id
        );

        let tunnel_secret = self.generate_tunnel_secret();
        let payload = serde_json::json!({
            "name": name,
            "tunnel_secret": tunnel_secret,
        });

        let response: CloudflareResponse<Tunnel> = self
            .client
            .post(&create_url)
            .json(&payload)
            .send()
            .await
            .context("Failed to create tunnel")?
            .json()
            .await
            .context("Failed to parse tunnel creation response")?;

        if !response.success {
            anyhow::bail!("Failed to create tunnel: {:?}", response.errors);
        }

        let mut tunnel = response.result;
        tunnel.secret = tunnel_secret;
        Ok(tunnel)
    }

    /// Create DNS CNAME record for tunnel
    pub async fn create_dns_record(
        &self,
        zone_name: &str,
        subdomain: &str,
        tunnel_id: &str,
    ) -> Result<()> {
        // Get zone ID from zone name
        let zones_url = format!("{}/zones?name={}", CLOUDFLARE_API_BASE, zone_name);
        
        #[derive(Deserialize)]
        struct Zone {
            id: String,
        }
        
        let zones_response: CloudflareResponse<Vec<Zone>> = self
            .client
            .get(&zones_url)
            .send()
            .await
            .context("Failed to fetch zone information")?
            .json()
            .await
            .context("Failed to parse zones response")?;

        let zone_id = zones_response
            .result
            .first()
            .context("Zone not found")?
            .id
            .clone();

        // Create DNS record
        let dns_url = format!("{}/zones/{}/dns_records", CLOUDFLARE_API_BASE, zone_id);
        let tunnel_cname = format!("{}.cfargotunnel.com", tunnel_id);
        
        let payload = serde_json::json!({
            "type": "CNAME",
            "name": subdomain,
            "content": tunnel_cname,
            "ttl": 1,
            "proxied": true,
        });

        let response: CloudflareResponse<serde_json::Value> = self
            .client
            .post(&dns_url)
            .json(&payload)
            .send()
            .await
            .context("Failed to create DNS record")?
            .json()
            .await
            .context("Failed to parse DNS creation response")?;

        if !response.success {
            // Check if record already exists
            if response.errors.iter().any(|e| e.code == 81057) {
                warn!("DNS record already exists, continuing...");
                return Ok(());
            }
            anyhow::bail!("Failed to create DNS record: {:?}", response.errors);
        }

        Ok(())
    }

    /// Create Zero Trust Access Application
    pub async fn create_access_application(&self, hostname: &str) -> Result<AccessApplication> {
        let url = format!(
            "{}/accounts/{}/access/apps",
            CLOUDFLARE_API_BASE, self.account_id
        );

        let payload = serde_json::json!({
            "name": format!("ACP Bridge - {}", hostname),
            "domain": hostname,
            "type": "self_hosted",
            "session_duration": "24h",
            "allowed_idps": [],
            "auto_redirect_to_identity": false,
        });

        let response: CloudflareResponse<AccessApplication> = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .context("Failed to create Access Application")?
            .json()
            .await
            .context("Failed to parse Access Application response")?;

        if !response.success {
            anyhow::bail!("Failed to create Access Application: {:?}", response.errors);
        }

        // Create Service Auth policy
        self.create_service_auth_policy(&response.result.id, hostname).await?;

        Ok(response.result)
    }

    /// Create Service Auth policy for the application
    async fn create_service_auth_policy(&self, app_id: &str, hostname: &str) -> Result<()> {
        let url = format!(
            "{}/accounts/{}/access/apps/{}/policies",
            CLOUDFLARE_API_BASE, self.account_id, app_id
        );

        let payload = serde_json::json!({
            "name": format!("Service Auth - {}", hostname),
            "decision": "non_identity",
            "include": [{
                "service_token": {}
            }],
            "precedence": 1,
        });

        let response: CloudflareResponse<serde_json::Value> = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .context("Failed to create Service Auth policy")?
            .json()
            .await
            .context("Failed to parse policy response")?;

        if !response.success {
            anyhow::bail!("Failed to create Service Auth policy: {:?}", response.errors);
        }

        Ok(())
    }

    /// Generate a Service Token for mobile authentication
    pub async fn create_service_token(&self, name: &str) -> Result<ServiceToken> {
        let url = format!(
            "{}/accounts/{}/access/service_tokens",
            CLOUDFLARE_API_BASE, self.account_id
        );

        let payload = serde_json::json!({
            "name": format!("Mobile Client - {}", name),
            "duration": "8760h", // 1 year
        });

        let response: CloudflareResponse<ServiceToken> = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .context("Failed to create Service Token")?
            .json()
            .await
            .context("Failed to parse Service Token response")?;

        if !response.success {
            anyhow::bail!("Failed to create Service Token: {:?}", response.errors);
        }

        Ok(response.result)
    }

    /// Configure tunnel ingress rules
    pub async fn configure_tunnel_ingress(
        &self,
        tunnel_id: &str,
        hostname: &str,
        local_port: u16,
    ) -> Result<()> {
        let url = format!(
            "{}/accounts/{}/cfd_tunnel/{}/configurations",
            CLOUDFLARE_API_BASE, self.account_id, tunnel_id
        );

        let payload = serde_json::json!({
            "config": {
                "ingress": [
                    {
                        "hostname": hostname,
                        "service": format!("http://localhost:{}", local_port),
                    },
                    {
                        "service": "http_status:404",
                    }
                ],
            }
        });

        let response: CloudflareResponse<serde_json::Value> = self
            .client
            .put(&url)
            .json(&payload)
            .send()
            .await
            .context("Failed to configure tunnel ingress")?
            .json()
            .await
            .context("Failed to parse ingress configuration response")?;

        if !response.success {
            anyhow::bail!("Failed to configure tunnel ingress: {:?}", response.errors);
        }

        Ok(())
    }

    /// Generate a secure tunnel secret
    fn generate_tunnel_secret(&self) -> String {
        use base64::{engine::general_purpose, Engine as _};
        let random_bytes: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
        general_purpose::STANDARD.encode(random_bytes)
    }
}

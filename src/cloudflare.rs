use anyhow::{Context, Result};
use reqwest::{Client, header};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

const CLOUDFLARE_API_BASE: &str = "https://api.cloudflare.com/client/v4";

/// Cloudflare API client for Zero Trust operations
pub struct CloudflareClient {
    client: Client,
    #[allow(dead_code)]
    api_token: String,
    account_id: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
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
    #[allow(dead_code)]
    messages: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CloudflareError {
    code: i32,
    #[allow(dead_code)]
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
            // Error 81053/81057: record with that name already exists — update it instead
            if response.errors.iter().any(|e| e.code == 81053 || e.code == 81057) {
                warn!("DNS record already exists, updating to point to current tunnel...");
                let full_hostname = format!("{}.{}", subdomain, zone_name);
                return self.update_dns_record(&zone_id, &full_hostname, &tunnel_cname).await;
            }
            anyhow::bail!("Failed to create DNS record: {:?}", response.errors);
        }

        Ok(())
    }

    /// Find and update an existing DNS CNAME record by name.
    async fn update_dns_record(&self, zone_id: &str, subdomain: &str, content: &str) -> Result<()> {
        #[derive(Deserialize)]
        struct DnsRecord {
            id: String,
        }

        let list_url = format!(
            "{}/zones/{}/dns_records?name={}&type=CNAME",
            CLOUDFLARE_API_BASE, zone_id, subdomain
        );

        let list_response: CloudflareResponse<Vec<DnsRecord>> = self
            .client
            .get(&list_url)
            .send()
            .await
            .context("Failed to list DNS records")?
            .json()
            .await
            .context("Failed to parse DNS records list")?;

        let record_id = list_response
            .result
            .first()
            .context("DNS record not found for update")?
            .id
            .clone();

        let update_url = format!("{}/zones/{}/dns_records/{}", CLOUDFLARE_API_BASE, zone_id, record_id);
        let payload = serde_json::json!({
            "type": "CNAME",
            "name": subdomain,
            "content": content,
            "ttl": 1,
            "proxied": true,
        });

        let response: CloudflareResponse<serde_json::Value> = self
            .client
            .put(&update_url)
            .json(&payload)
            .send()
            .await
            .context("Failed to update DNS record")?
            .json()
            .await
            .context("Failed to parse DNS update response")?;

        if !response.success {
            anyhow::bail!("Failed to update DNS record: {:?}", response.errors);
        }

        info!("✅ DNS record updated to point to current tunnel");
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

        let response: CloudflareResponse<Option<AccessApplication>> = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .context("Failed to create Access Application")?
            .json()
            .await
            .context("Failed to parse Access Application response")?;

        if !response.success || response.result.is_none() {
            warn!("Access Application creation failed, checking for existing app...");
            let app = self.find_access_application(hostname).await?;
            // Policy may already exist; ignore errors from duplicate policy creation
            let _ = self.create_service_auth_policy(&app.id, hostname).await;
            return Ok(app);
        }

        let app = response.result.unwrap();
        // Create Service Auth policy
        self.create_service_auth_policy(&app.id, hostname).await?;
        Ok(app)
    }

    /// Find an existing Access Application by hostname.
    async fn find_access_application(&self, hostname: &str) -> Result<AccessApplication> {
        let url = format!(
            "{}/accounts/{}/access/apps",
            CLOUDFLARE_API_BASE, self.account_id
        );

        let response: CloudflareResponse<Vec<AccessApplication>> = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to list Access Applications")?
            .json()
            .await
            .context("Failed to parse Access Applications list")?;

        response
            .result
            .into_iter()
            .find(|app| app.domain == hostname)
            .with_context(|| format!("No Access Application found for hostname: {}", hostname))
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
                "any_valid_service_token": {}
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
            // Ignore "already exists" type errors — policy from a previous run is fine
            let already_exists = response.errors.iter().any(|e| {
                e.message.contains("already exists") || e.message.contains("duplicate")
            });
            if already_exists {
                warn!("Service Auth policy already exists, skipping...");
                return Ok(());
            }
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

    /// Get the account ID for this client
    #[allow(dead_code)]
    pub fn account_id(&self) -> &str {
        &self.account_id
    }
}

/// Write the cloudflared tunnel credentials JSON file to ~/.cloudflared/<tunnel-id>.json.
/// This file is required by `cloudflared tunnel run` to authenticate to Cloudflare.
pub fn write_credentials_file(
    account_id: &str,
    tunnel_id: &str,
    tunnel_secret: &str,
) -> Result<std::path::PathBuf> {
    let cloudflared_dir = get_cloudflared_dir()?;
    std::fs::create_dir_all(&cloudflared_dir)
        .context("Failed to create ~/.cloudflared directory")?;

    let credentials_path = cloudflared_dir.join(format!("{}.json", tunnel_id));
    let credentials = serde_json::json!({
        "AccountTag": account_id,
        "TunnelSecret": tunnel_secret,
        "TunnelID": tunnel_id,
    });
    std::fs::write(&credentials_path, serde_json::to_string_pretty(&credentials)?)
        .context("Failed to write tunnel credentials file")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&credentials_path, perms)?;
    }

    Ok(credentials_path)
}

/// Write the cloudflared config.yml to ~/.cloudflared/config.yml.
/// This configures which tunnel to run and the ingress rules.
pub fn write_cloudflared_config(
    tunnel_id: &str,
    credentials_path: &std::path::Path,
    hostname: &str,
    local_port: u16,
) -> Result<std::path::PathBuf> {
    let cloudflared_dir = get_cloudflared_dir()?;
    std::fs::create_dir_all(&cloudflared_dir)
        .context("Failed to create ~/.cloudflared directory")?;

    let config_path = cloudflared_dir.join("config.yml");
    let credentials_str = credentials_path.to_string_lossy();
    let config_content = format!(
        "tunnel: {tunnel_id}\n\
         credentials-file: {credentials_str}\n\
         \n\
         ingress:\n\
           - hostname: {hostname}\n\
             service: http://localhost:{local_port}\n\
           - service: http_status:404\n"
    );
    std::fs::write(&config_path, &config_content)
        .context("Failed to write cloudflared config.yml")?;

    Ok(config_path)
}

/// Return the path to the cloudflared config YAML (does not check existence).
pub fn cloudflared_config_path() -> Result<std::path::PathBuf> {
    Ok(get_cloudflared_dir()?.join("config.yml"))
}

/// Return the path to the cloudflared credentials file for a given tunnel ID.
#[allow(dead_code)]
pub fn cloudflared_credentials_path(tunnel_id: &str) -> Result<std::path::PathBuf> {
    Ok(get_cloudflared_dir()?.join(format!("{}.json", tunnel_id)))
}

fn get_cloudflared_dir() -> Result<std::path::PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("Cannot determine home directory (HOME not set)")?;
    Ok(std::path::PathBuf::from(home).join(".cloudflared"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn fake_cloudflared_dir(tmp: &TempDir) -> std::path::PathBuf {
        let dir = tmp.path().join(".cloudflared");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn credentials_file_has_required_fields() {
        let tmp = TempDir::new().unwrap();
        // Override HOME so write_credentials_file uses tmp dir
        std::env::set_var("HOME", tmp.path().to_str().unwrap());

        let path = write_credentials_file("acct123", "tunnel-abc", "secret-base64==").unwrap();
        assert!(path.exists(), "credentials file should be created");

        let content = fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(json["AccountTag"], "acct123");
        assert_eq!(json["TunnelSecret"], "secret-base64==");
        assert_eq!(json["TunnelID"], "tunnel-abc");
    }

    #[test]
    fn config_yml_has_correct_sections() {
        let tmp = TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path().to_str().unwrap());

        let creds_path = fake_cloudflared_dir(&tmp).join("tunnel-abc.json");
        fs::write(&creds_path, "{}").unwrap();

        let config_path = write_cloudflared_config(
            "tunnel-abc",
            &creds_path,
            "agent.example.com",
            8080,
        )
        .unwrap();

        let content = fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("tunnel: tunnel-abc"), "should have tunnel ID");
        assert!(content.contains("credentials-file:"), "should have credentials-file");
        assert!(content.contains("hostname: agent.example.com"), "should have hostname");
        assert!(content.contains("http://localhost:8080"), "should have local port");
        assert!(content.contains("http_status:404"), "should have fallback rule");
    }
}

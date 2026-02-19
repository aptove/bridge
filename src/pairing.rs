use rand::Rng;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use thiserror::Error;

/// Errors that can occur during pairing
#[derive(Error, Debug)]
pub enum PairingError {
    #[error("Pairing code is invalid or expired")]
    InvalidCode,
    #[error("Pairing code has already been used")]
    CodeAlreadyUsed,
    #[error("Too many failed attempts. Please restart the bridge to get a new code.")]
    RateLimited,
}

/// Result type for pairing response
#[derive(serde::Serialize)]
pub struct PairingResponse {
    pub url: String,
    pub protocol: String,
    pub version: String,
    #[serde(rename = "authToken")]
    pub auth_token: String,
    #[serde(rename = "certFingerprint", skip_serializing_if = "Option::is_none")]
    pub cert_fingerprint: Option<String>,
    #[serde(rename = "clientId", skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(rename = "clientSecret", skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

/// Error response for failed pairing attempts
#[derive(serde::Serialize)]
pub struct PairingErrorResponse {
    pub error: String,
    pub message: String,
}

impl PairingErrorResponse {
    pub fn invalid_code() -> Self {
        Self {
            error: "invalid_code".to_string(),
            message: "Pairing code is invalid or expired".to_string(),
        }
    }

    pub fn rate_limited() -> Self {
        Self {
            error: "rate_limited".to_string(),
            message: "Too many failed attempts. Please restart the bridge to get a new code.".to_string(),
        }
    }
}

/// Manages one-time pairing codes for secure client registration
pub struct PairingManager {
    /// Current 6-digit pairing code
    code: String,
    /// When the code was created (for expiration)
    created_at: Instant,
    /// Whether the code has been successfully used
    used: AtomicBool,
    /// Number of failed validation attempts (for rate limiting)
    attempts: AtomicU32,
    /// Connection details to return on successful pairing
    websocket_url: String,
    auth_token: String,
    cert_fingerprint: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    /// Code expiration duration
    expiry_duration: Duration,
    /// Maximum failed attempts before rate limiting
    max_attempts: u32,
}

impl PairingManager {
    /// Create a new PairingManager with a fresh pairing code
    pub fn new(
        websocket_url: String,
        auth_token: String,
        cert_fingerprint: Option<String>,
    ) -> Self {
        Self::new_with_cf(websocket_url, auth_token, cert_fingerprint, None, None)
    }

    /// Create a new PairingManager including Cloudflare service token credentials
    pub fn new_with_cf(
        websocket_url: String,
        auth_token: String,
        cert_fingerprint: Option<String>,
        client_id: Option<String>,
        client_secret: Option<String>,
    ) -> Self {
        let code = generate_pairing_code();
        Self {
            code,
            created_at: Instant::now(),
            used: AtomicBool::new(false),
            attempts: AtomicU32::new(0),
            websocket_url,
            auth_token,
            cert_fingerprint,
            client_id,
            client_secret,
            expiry_duration: Duration::from_secs(60),
            max_attempts: 5,
        }
    }

    /// Get the current pairing code
    #[allow(dead_code)]
    pub fn get_code(&self) -> &str {
        &self.code
    }

    /// Get the pairing URL (for QR code)
    pub fn get_pairing_url(&self, base_url: &str) -> String {
        let mut url = format!("{}/pair/local?code={}", base_url, self.code);
        if let Some(ref fp) = self.cert_fingerprint {
            // URL-encode the fingerprint (colons are safe, but good practice)
            url.push_str("&fp=");
            url.push_str(&urlencoding::encode(fp));
        }
        url
    }

    /// Check if the code has expired
    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() > self.expiry_duration
    }

    /// Check if the code has been used
    #[allow(dead_code)]
    pub fn is_used(&self) -> bool {
        self.used.load(Ordering::SeqCst)
    }

    /// Get remaining seconds until expiration
    pub fn seconds_remaining(&self) -> u64 {
        let elapsed = self.created_at.elapsed();
        if elapsed > self.expiry_duration {
            0
        } else {
            (self.expiry_duration - elapsed).as_secs()
        }
    }

    /// Validate a pairing code and return connection details if valid
    pub fn validate(&self, code: &str) -> Result<PairingResponse, PairingError> {
        // Check rate limiting first
        let attempts = self.attempts.load(Ordering::SeqCst);
        if attempts >= self.max_attempts {
            return Err(PairingError::RateLimited);
        }

        // Check if already used
        if self.used.load(Ordering::SeqCst) {
            return Err(PairingError::CodeAlreadyUsed);
        }

        // Check expiration
        if self.is_expired() {
            return Err(PairingError::InvalidCode);
        }

        // Validate code
        if code != self.code {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            return Err(PairingError::InvalidCode);
        }

        // Mark as used
        if self.used.swap(true, Ordering::SeqCst) {
            // Another thread already used it
            return Err(PairingError::CodeAlreadyUsed);
        }

        Ok(PairingResponse {
            url: self.websocket_url.clone(),
            protocol: "acp".to_string(),
            version: "1.0".to_string(),
            auth_token: self.auth_token.clone(),
            cert_fingerprint: self.cert_fingerprint.clone(),
            client_id: self.client_id.clone(),
            client_secret: self.client_secret.clone(),
        })
    }

    /// Get the certificate fingerprint (if available)
    #[allow(dead_code)]
    pub fn get_cert_fingerprint(&self) -> Option<&str> {
        self.cert_fingerprint.as_deref()
    }
}

/// Generate a cryptographically random 6-digit pairing code
fn generate_pairing_code() -> String {
    let mut rng = rand::thread_rng();
    let code: u32 = rng.gen_range(100000..1000000);
    code.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_generation() {
        let code = generate_pairing_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn test_pairing_manager_valid_code() {
        let manager = PairingManager::new(
            "wss://192.168.1.100:8080".to_string(),
            "test-token".to_string(),
            Some("SHA256:ABC123".to_string()),
        );

        let code = manager.get_code().to_string();
        let result = manager.validate(&code);
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response.url, "wss://192.168.1.100:8080");
        assert_eq!(response.auth_token, "test-token");
    }

    #[test]
    fn test_pairing_manager_invalid_code() {
        let manager = PairingManager::new(
            "wss://192.168.1.100:8080".to_string(),
            "test-token".to_string(),
            None,
        );

        let result = manager.validate("000000");
        assert!(matches!(result, Err(PairingError::InvalidCode)));
    }

    #[test]
    fn test_pairing_manager_one_time_use() {
        let manager = PairingManager::new(
            "wss://192.168.1.100:8080".to_string(),
            "test-token".to_string(),
            None,
        );

        let code = manager.get_code().to_string();
        
        // First use should succeed
        assert!(manager.validate(&code).is_ok());
        
        // Second use should fail
        let result = manager.validate(&code);
        assert!(matches!(result, Err(PairingError::CodeAlreadyUsed)));
    }

    #[test]
    fn test_pairing_manager_rate_limiting() {
        let manager = PairingManager::new(
            "wss://192.168.1.100:8080".to_string(),
            "test-token".to_string(),
            None,
        );

        // Make 5 failed attempts
        for _ in 0..5 {
            let _ = manager.validate("000000");
        }

        // Next attempt should be rate limited
        let result = manager.validate("000000");
        assert!(matches!(result, Err(PairingError::RateLimited)));
    }

    #[test]
    fn test_pairing_url_generation() {
        let manager = PairingManager::new(
            "wss://192.168.1.100:8080".to_string(),
            "test-token".to_string(),
            Some("SHA256:ABC123".to_string()),
        );

        let url = manager.get_pairing_url("https://192.168.1.100:8080");
        assert!(url.starts_with("https://192.168.1.100:8080/pair/local?code="));
        assert!(url.contains("&fp=SHA256"));
    }
}

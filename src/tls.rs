use anyhow::{Context, Result};
use rcgen::{Certificate, CertificateParams, DnType, SanType};
use sha2::{Sha256, Digest};
use std::fs;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_rustls::rustls;
use tracing::{info, warn};

const CERT_FILENAME: &str = "cert.pem";
const KEY_FILENAME: &str = "key.pem";
const EXTRA_SANS_FILENAME: &str = "cert-extra-sans.json";

/// TLS configuration for the bridge
pub struct TlsConfig {
    /// Path to the certificate file
    #[allow(dead_code)]
    pub cert_path: PathBuf,
    /// Path to the private key file
    #[allow(dead_code)]
    pub key_path: PathBuf,
    /// SHA256 fingerprint of the certificate (hex encoded)
    pub fingerprint: String,
    /// TLS acceptor for incoming connections
    pub acceptor: tokio_rustls::TlsAcceptor,
}

impl TlsConfig {
    /// Load or generate TLS configuration.
    /// `extra_sans` is a list of additional IP addresses or DNS names to include in the certificate SANs.
    pub fn load_or_generate(config_dir: &PathBuf, extra_sans: &[String]) -> Result<Self> {
        let cert_path = config_dir.join(CERT_FILENAME);
        let key_path = config_dir.join(KEY_FILENAME);
        let extra_sans_path = config_dir.join(EXTRA_SANS_FILENAME);

        // If cert exists, check whether extra_sans have changed
        if cert_path.exists() && key_path.exists() {
            if !extra_sans.is_empty() {
                let mut sorted = extra_sans.to_vec();
                sorted.sort();
                let current_json = serde_json::to_string(&sorted).unwrap_or_default();

                let stored_json = fs::read_to_string(&extra_sans_path).unwrap_or_default();
                if stored_json.trim() != current_json.trim() {
                    warn!("⚠️  Tailscale address changed since last certificate generation. Regenerating TLS certificate (mobile app will need to re-pair).");
                    let _ = fs::remove_file(&cert_path);
                    let _ = fs::remove_file(&key_path);
                }
            }
        }

        if cert_path.exists() && key_path.exists() {
            info!("🔐 Loading existing TLS certificate");
            Self::load_existing(&cert_path, &key_path)
        } else {
            info!("🔐 Generating new self-signed TLS certificate");
            let result = Self::generate_new(&cert_path, &key_path, extra_sans)?;
            // Persist extra_sans for future change detection
            if !extra_sans.is_empty() {
                let mut sorted = extra_sans.to_vec();
                sorted.sort();
                let json = serde_json::to_string(&sorted).unwrap_or_default();
                let _ = fs::write(&extra_sans_path, json);
            }
            Ok(result)
        }
    }

    /// Load existing certificate and key
    fn load_existing(cert_path: &PathBuf, key_path: &PathBuf) -> Result<Self> {
        let cert_pem = fs::read_to_string(cert_path)
            .context("Failed to read certificate file")?;
        let key_pem = fs::read_to_string(key_path)
            .context("Failed to read private key file")?;

        let fingerprint = Self::calculate_fingerprint(&cert_pem)?;
        let acceptor = Self::create_acceptor(&cert_pem, &key_pem)?;

        Ok(Self {
            cert_path: cert_path.clone(),
            key_path: key_path.clone(),
            fingerprint,
            acceptor,
        })
    }

    /// Generate new self-signed certificate
    fn generate_new(cert_path: &PathBuf, key_path: &PathBuf, extra_sans: &[String]) -> Result<Self> {
        // Set up certificate parameters
        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, "ACP Bridge");
        params.distinguished_name.push(DnType::OrganizationName, "Local Development");

        // Add base SANs for local connections
        params.subject_alt_names = vec![
            SanType::DnsName("localhost".try_into().unwrap()),
            SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))),
        ];

        // Add local network IP
        if let Ok(local_ip) = local_ip_address::local_ip() {
            params.subject_alt_names.push(SanType::IpAddress(local_ip));
        }

        // Add extra SANs (Tailscale IP/hostname, etc.)
        for san in extra_sans {
            if let Ok(ip) = san.parse::<IpAddr>() {
                params.subject_alt_names.push(SanType::IpAddress(ip));
            } else {
                let dns = san.clone().try_into();
                match dns {
                    Ok(d) => params.subject_alt_names.push(SanType::DnsName(d)),
                    Err(_) => warn!("Skipping invalid SAN '{}': not a valid IP or DNS name", san),
                }
            }
        }
        
        // Valid for 1 year
        params.not_before = time::OffsetDateTime::now_utc();
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(365);

        // Generate self-signed certificate (key pair is auto-generated)
        let cert = Certificate::from_params(params)
            .context("Failed to generate self-signed certificate")?;

        let cert_pem = cert.serialize_pem()
            .context("Failed to serialize certificate to PEM")?;
        let key_pem = cert.serialize_private_key_pem();

        // Save to files
        fs::write(cert_path, &cert_pem)
            .context("Failed to write certificate file")?;
        fs::write(key_path, &key_pem)
            .context("Failed to write private key file")?;

        // Set restrictive permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(cert_path, perms.clone())?;
            fs::set_permissions(key_path, perms)?;
        }

        info!("✅ TLS certificate generated and saved");

        let fingerprint = Self::calculate_fingerprint(&cert_pem)?;
        let acceptor = Self::create_acceptor(&cert_pem, &key_pem)?;

        Ok(Self {
            cert_path: cert_path.clone(),
            key_path: key_path.clone(),
            fingerprint,
            acceptor,
        })
    }

    /// Calculate SHA256 fingerprint of certificate
    fn calculate_fingerprint(cert_pem: &str) -> Result<String> {
        // Parse PEM to get DER bytes
        let mut reader = std::io::BufReader::new(cert_pem.as_bytes());
        let certs = rustls_pemfile::certs(&mut reader)
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to parse certificate PEM")?;

        let cert_der = certs.first()
            .context("No certificate found in PEM")?;

        // Calculate SHA256 hash
        let mut hasher = Sha256::new();
        hasher.update(cert_der.as_ref());
        let hash = hasher.finalize();

        // Format as hex with colons (e.g., "AB:CD:EF:...")
        let fingerprint = hash.iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(":");

        Ok(fingerprint)
    }

    /// Create TLS acceptor from PEM strings
    fn create_acceptor(cert_pem: &str, key_pem: &str) -> Result<tokio_rustls::TlsAcceptor> {
        // Parse certificate
        let mut cert_reader = std::io::BufReader::new(cert_pem.as_bytes());
        let certs = rustls_pemfile::certs(&mut cert_reader)
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to parse certificate")?;

        // Parse private key
        let mut key_reader = std::io::BufReader::new(key_pem.as_bytes());
        let key = rustls_pemfile::private_key(&mut key_reader)
            .context("Failed to read private key")?
            .context("No private key found")?;

        // Build TLS config
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("Failed to build TLS config")?;

        Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
    }

    /// Get the fingerprint in a format suitable for display
    pub fn fingerprint_short(&self) -> String {
        // Return first 16 chars (8 bytes) for brevity
        self.fingerprint.chars().take(23).collect()
    }
}

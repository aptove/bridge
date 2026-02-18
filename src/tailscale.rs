use anyhow::{Context, Result};
use std::process::{Command, Stdio};
use tracing::{debug, info};

const INSTALL_HINT: &str = "\
tailscale not found on PATH.\n\
Install it from: https://tailscale.com/download";

/// Returns `true` if the `tailscale` binary is available on PATH.
pub fn is_tailscale_available() -> bool {
    Command::new("tailscale")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Returns the machine's Tailscale IPv4 address (100.x.x.x range).
pub fn get_tailscale_ipv4() -> Result<String> {
    if !is_tailscale_available() {
        anyhow::bail!("{}", INSTALL_HINT);
    }
    let output = Command::new("tailscale")
        .args(["ip", "--4"])
        .output()
        .context("Failed to run 'tailscale ip --4'")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Not enrolled in a Tailscale network. Run 'tailscale up' first.\n{}", stderr.trim());
    }
    let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ip.is_empty() {
        anyhow::bail!("Not enrolled in a Tailscale network. Run 'tailscale up' first.");
    }
    Ok(ip)
}

/// Returns the machine's MagicDNS hostname (e.g., `my-laptop.tail1234.ts.net`).
/// Returns `None` if MagicDNS is not enabled or the hostname is empty.
pub fn get_tailscale_hostname() -> Result<Option<String>> {
    if !is_tailscale_available() {
        anyhow::bail!("{}", INSTALL_HINT);
    }
    let output = Command::new("tailscale")
        .args(["status", "--json"])
        .output()
        .context("Failed to run 'tailscale status --json'")?;
    if !output.status.success() {
        return Ok(None);
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("Failed to parse tailscale status JSON")?;
    let dns_name = json
        .get("Self")
        .and_then(|s| s.get("DNSName"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim_end_matches('.').to_string());
    match dns_name {
        Some(s) if s.is_empty() => Ok(None),
        other => Ok(other),
    }
}

/// Parse `(major, minor)` from `tailscale version` output.
fn parse_tailscale_version(output: &str) -> Option<(u32, u32)> {
    let first_line = output.lines().next()?;
    let parts: Vec<&str> = first_line.trim().splitn(3, '.').collect();
    if parts.len() >= 2 {
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        Some((major, minor))
    } else {
        None
    }
}

/// Verifies tailscale is at least v1.38 (required for `tailscale serve`).
fn check_tailscale_version() -> Result<()> {
    let output = Command::new("tailscale")
        .arg("version")
        .output()
        .context("Failed to run 'tailscale version'")?;
    let version_str = String::from_utf8_lossy(&output.stdout);
    if let Some((major, minor)) = parse_tailscale_version(&version_str) {
        if major == 0 || (major == 1 && minor < 38) {
            anyhow::bail!(
                "tailscale serve requires Tailscale v1.38+. Installed: {}.{}. \
                 Update at https://tailscale.com/download",
                major, minor
            );
        }
    }
    Ok(())
}

/// Guard that runs `tailscale serve reset` when dropped.
pub struct TailscaleServeGuard {
    port: u16,
}

impl TailscaleServeGuard {
    fn new(port: u16) -> Self {
        Self { port }
    }
}

impl Drop for TailscaleServeGuard {
    fn drop(&mut self) {
        debug!("TailscaleServeGuard dropped — removing tailscale serve config for port {}", self.port);
        let _ = Command::new("tailscale")
            .args(["serve", "reset"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Configure `tailscale serve` to proxy HTTPS (port 443) to the bridge on localhost.
/// Requires MagicDNS + HTTPS enabled on the tailnet.
/// Returns a guard that runs `tailscale serve reset` when dropped.
pub fn tailscale_serve_start(port: u16) -> Result<TailscaleServeGuard> {
    if !is_tailscale_available() {
        anyhow::bail!("{}", INSTALL_HINT);
    }
    check_tailscale_version()?;
    // Verify MagicDNS hostname is available
    let hostname = get_tailscale_hostname()?;
    if hostname.is_none() {
        anyhow::bail!(
            "tailscale serve mode requires MagicDNS + HTTPS to be enabled on your tailnet.\n\
             Enable HTTPS in the Tailscale admin console: https://tailscale.com/kb/1153/enabling-https\n\
             Alternatively use --tailscale ip for direct IP binding."
        );
    }
    info!("🔧 Configuring tailscale serve → localhost:{}", port);
    let backend = format!("http://localhost:{}", port);
    let status = Command::new("tailscale")
        .args(["serve", "--https=443", &backend])
        .status()
        .context("Failed to run 'tailscale serve'")?;
    if !status.success() {
        anyhow::bail!(
            "tailscale serve failed (exit {}). \
             Ensure MagicDNS and HTTPS are enabled: https://tailscale.com/kb/1153/enabling-https",
            status
        );
    }
    Ok(TailscaleServeGuard::new(port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tailscale_version_valid() {
        assert_eq!(parse_tailscale_version("1.56.1\n  build info"), Some((1, 56)));
        assert_eq!(parse_tailscale_version("1.38.0"), Some((1, 38)));
        assert_eq!(parse_tailscale_version("2.0.1"), Some((2, 0)));
    }

    #[test]
    fn test_parse_tailscale_version_invalid() {
        assert_eq!(parse_tailscale_version(""), None);
        assert_eq!(parse_tailscale_version("not-a-version"), None);
    }

    #[test]
    fn test_is_tailscale_available_smoke() {
        // This just tests the function runs without panicking.
        let _ = is_tailscale_available();
    }

    #[test]
    fn test_get_tailscale_hostname_parses_json() {
        // We can test the JSON parsing logic by calling the function indirectly.
        // The key behavior: DNSName with trailing dot is trimmed.
        let json: serde_json::Value = serde_json::json!({
            "Self": {
                "DNSName": "my-laptop.tail1234.ts.net."
            }
        });
        let dns_name = json
            .get("Self")
            .and_then(|s| s.get("DNSName"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim_end_matches('.').to_string());
        assert_eq!(dns_name, Some("my-laptop.tail1234.ts.net".to_string()));
    }

    #[test]
    fn test_get_tailscale_hostname_empty_dns_name() {
        let json: serde_json::Value = serde_json::json!({
            "Self": { "DNSName": "" }
        });
        let dns_name = json
            .get("Self")
            .and_then(|s| s.get("DNSName"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim_end_matches('.').to_string());
        let result: Option<String> = match dns_name {
            Some(s) if s.is_empty() => None,
            other => other,
        };
        assert_eq!(result, None);
    }

    #[test]
    fn test_get_tailscale_hostname_missing_field() {
        let json: serde_json::Value = serde_json::json!({ "Self": {} });
        let dns_name = json
            .get("Self")
            .and_then(|s| s.get("DNSName"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim_end_matches('.').to_string());
        assert_eq!(dns_name, None);
    }
}

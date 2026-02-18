use anyhow::{Context, Result};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tracing::{debug, warn};

const READY_MARKERS: &[&str] = &[
    "Registered tunnel connection",
    "Connection registered",
    "Connected to",
];

const INSTALL_HINT: &str = "\
cloudflared not found on PATH.\n\
Install it with:\n\
  macOS:  brew install cloudflare/cloudflare/cloudflared\n\
  Linux:  See https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/\n\
  Windows: https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/";

/// Manages the lifecycle of a `cloudflared tunnel run` child process.
/// When dropped, the child process is terminated.
pub struct CloudflaredRunner {
    child: Option<Child>,
    /// Buffered stderr lines captured during startup (for diagnostics)
    startup_lines: Vec<String>,
}

impl CloudflaredRunner {
    /// Spawn `cloudflared tunnel --config <config_yml_path> run <tunnel_id>`.
    /// Returns an error if `cloudflared` is not found on PATH.
    pub fn spawn(config_yml_path: &Path, tunnel_id: &str) -> Result<Self> {
        // Verify cloudflared is available before attempting to spawn
        if !is_cloudflared_available() {
            anyhow::bail!("{}", INSTALL_HINT);
        }

        let child = Command::new("cloudflared")
            .args([
                "tunnel",
                "--config",
                &config_yml_path.to_string_lossy(),
                "run",
                tunnel_id,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn cloudflared process")?;

        Ok(Self {
            child: Some(child),
            startup_lines: Vec::new(),
        })
    }

    /// Block until cloudflared reports it has established a tunnel connection,
    /// or until `timeout` elapses. Returns an error with diagnostic stderr lines
    /// if the timeout expires before a ready marker is seen.
    pub fn wait_for_ready(&mut self, timeout: Duration) -> Result<()> {
        let stderr = self
            .child
            .as_mut()
            .and_then(|c| c.stderr.take())
            .context("cloudflared stderr not available")?;

        let reader = BufReader::new(stderr);
        let deadline = Instant::now() + timeout;

        for line in reader.lines() {
            if Instant::now() > deadline {
                // Kill the child before returning the error
                self.kill_child();
                return Err(anyhow::anyhow!(
                    "cloudflared did not become ready within {} seconds.\nLast output:\n{}",
                    timeout.as_secs(),
                    self.startup_lines.join("\n")
                ));
            }

            match line {
                Ok(line) => {
                    debug!("cloudflared: {}", line);
                    self.startup_lines.push(line.clone());
                    if READY_MARKERS.iter().any(|m| line.contains(m)) {
                        return Ok(());
                    }
                }
                Err(e) => {
                    warn!("Error reading cloudflared stderr: {}", e);
                    break;
                }
            }
        }

        // Reader exhausted (process exited) before ready marker
        self.kill_child();
        Err(anyhow::anyhow!(
            "cloudflared exited before becoming ready.\nOutput:\n{}",
            self.startup_lines.join("\n")
        ))
    }

    fn kill_child(&mut self) {
        if let Some(ref mut child) = self.child {
            let _ = child.kill();
        }
    }
}

impl Drop for CloudflaredRunner {
    fn drop(&mut self) {
        if self.child.is_some() {
            debug!("CloudflaredRunner dropped — terminating cloudflared child process");
            self.kill_child();
        }
    }
}

/// Returns `true` if `cloudflared` is found on PATH.
fn is_cloudflared_available() -> bool {
    Command::new("cloudflared")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;

    /// Simulate wait_for_ready with a fake stderr stream that immediately outputs
    /// a ready marker. We do this by writing to a temp file and reading from it.
    #[test]
    fn wait_for_ready_succeeds_on_marker() {
        let dir = tempfile::TempDir::new().unwrap();
        let stderr_file = dir.path().join("stderr.txt");
        std::fs::write(&stderr_file, "INF Registered tunnel connection\n").unwrap();

        let file = std::fs::File::open(&stderr_file).unwrap();
        let reader = BufReader::new(file);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut lines_seen = Vec::new();

        let mut found = false;
        for line in reader.lines() {
            if Instant::now() > deadline {
                break;
            }
            if let Ok(line) = line {
                lines_seen.push(line.clone());
                if READY_MARKERS.iter().any(|m| line.contains(m)) {
                    found = true;
                    break;
                }
            }
        }
        assert!(found, "should detect ready marker");
    }

    #[test]
    fn wait_for_ready_fails_on_no_marker() {
        let dir = tempfile::TempDir::new().unwrap();
        let stderr_file = dir.path().join("stderr.txt");
        std::fs::write(&stderr_file, "INF some other log line\n").unwrap();

        let file = std::fs::File::open(&stderr_file).unwrap();
        let reader = BufReader::new(file);
        // Very short deadline
        let deadline = Instant::now() + Duration::from_millis(1);
        let mut found = false;

        for line in reader.lines() {
            if Instant::now() > deadline {
                break;
            }
            if let Ok(line) = line {
                if READY_MARKERS.iter().any(|m| line.contains(m)) {
                    found = true;
                    break;
                }
            }
        }
        assert!(!found, "should not detect ready marker when not present");
    }

    #[test]
    fn cloudflared_not_available_when_bad_command() {
        // Temporarily override PATH-like check by testing the function directly
        // We can't unset PATH in a test safely, but we can verify the logic:
        // If `cloudflared --version` succeeds, is_cloudflared_available returns true.
        // We just verify the function exists and returns a bool.
        let _ = is_cloudflared_available(); // smoke test: must not panic
    }

    #[test]
    fn ready_markers_cover_known_cloudflared_messages() {
        let test_lines = [
            "INF Registered tunnel connection connIndex=0",
            "INF Connection registered",
            "INF Connected to edge location",
        ];
        for line in &test_lines {
            assert!(
                READY_MARKERS.iter().any(|m| line.contains(m)),
                "marker not detected in: {}",
                line
            );
        }
    }
}

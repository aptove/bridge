/// Basic connectivity check for an ACP agent.
///
/// Spawns the agent process, sends a `session/new` JSON-RPC request over
/// stdin, and reads the first response line from stdout.  Used in the
/// Configure Agent wizard step so the user can verify their command before
/// the bridge starts.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// The JSON-RPC 2.0 message sent to the agent as the connectivity test.
pub const TEST_MSG: &str =
    r#"{"jsonrpc":"2.0","id":"aptove-test","method":"session/new","params":{}}"#;

/// Documentation URL shown in the Configure Agent panel.
pub const AGENT_DOCS_URL: &str = "https://docs.aptove.com/bridge/agent";

/// Spawn `command`, send [`TEST_MSG`], and return the first response line.
///
/// Returns `Ok(pretty_response)` on success, `Err(description)` on failure
/// or timeout.
pub async fn run_agent_test(command: &str) -> Result<String, String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let (&program, args) = match parts.split_first() {
        Some(p) => p,
        None => return Err("Empty command".to_string()),
    };

    let result = timeout(Duration::from_secs(10), async {
        let mut child = Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start '{}': {}", command, e))?;

        let stdin = child.stdin.take()
            .ok_or_else(|| "Could not open agent stdin".to_string())?;
        let stdout = child.stdout.take()
            .ok_or_else(|| "Could not open agent stdout".to_string())?;

        let mut writer = tokio::io::BufWriter::new(stdin);
        let mut lines = BufReader::new(stdout).lines();

        writer
            .write_all(format!("{}\n", TEST_MSG).as_bytes())
            .await
            .map_err(|e| format!("Write error: {}", e))?;
        writer.flush().await.map_err(|e| format!("Flush error: {}", e))?;

        let line = timeout(Duration::from_secs(8), lines.next_line())
            .await
            .map_err(|_| "Timeout: no response within 8 seconds".to_string())?
            .map_err(|e| format!("Read error: {}", e))?
            .ok_or_else(|| "Agent closed stdout without responding".to_string())?;

        let _ = child.kill().await;

        // Pretty-print JSON if possible.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
            Ok(serde_json::to_string_pretty(&v).unwrap_or(line))
        } else {
            Ok(line)
        }
    })
    .await;

    match result {
        Ok(r) => r,
        Err(_) => Err("Timeout: agent did not respond within 10 seconds".to_string()),
    }
}

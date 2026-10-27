/// Basic connectivity check for an ACP agent.
///
/// Performs the full ACP handshake (initialize → session/new → session/prompt)
/// and collects the AI's text response from session/update notifications.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::{timeout, Duration, Instant};

/// The prompt text sent to the agent during the connectivity test.
pub const TEST_PROMPT_TEXT: &str = "Hello, what is your name?";

/// Documentation URL shown in the Configure Agent panel.
pub const AGENT_DOCS_URL: &str = "https://docs.aptove.com/bridge/agent";

/// Spawn `command`, run the ACP handshake, send a test prompt, and collect the response.
///
/// Returns `(sent_json, result)`:
/// - `sent_json`: the `session/prompt` message actually sent (placeholder on early failure)
/// - `result`: `Ok(response_text)` on success, `Err(description)` on any failure
pub async fn run_agent_test(command: &str) -> (String, Result<String, String>) {
    let parts: Vec<String> = command.split_whitespace().map(str::to_string).collect();
    let (program, args) = match parts.split_first() {
        Some((p, rest)) => (p.clone(), rest.to_vec()),
        None => return (String::new(), Err("Empty command".to_string())),
    };

    let outcome = timeout(
        Duration::from_secs(60),
        do_test(program, args, command.to_string()),
    )
    .await;

    match outcome {
        Ok(pair) => pair,
        Err(_) => (
            make_prompt_json("..."),
            Err("Timeout: agent did not complete within 60 seconds".to_string()),
        ),
    }
}

async fn do_test(program: String, args: Vec<String>, command: String) -> (String, Result<String, String>) {
    let mut child = match Command::new(&program)
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                make_prompt_json("..."),
                Err(format!("Failed to start '{}': {}", command, e)),
            )
        }
    };

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut writer = tokio::io::BufWriter::new(stdin);
    let mut lines = BufReader::new(stdout).lines();

    // ── Step 1: initialize ──────────────────────────────────────────────────
    let init_msg = concat!(
        r#"{"jsonrpc":"2.0","id":"aptove-init","method":"initialize","#,
        r#""params":{"protocolVersion":1,"clientCapabilities":{},"#,
        r#""clientInfo":{"name":"Aptove Bridge","version":"1.0"}}}"#
    );
    if let Err(e) = write_msg(&mut writer, init_msg).await {
        let _ = child.kill().await;
        return (make_prompt_json("..."), Err(e));
    }
    let init_resp = match read_line(&mut lines, 10).await {
        Ok(line) => line,
        Err(e) => {
            let _ = child.kill().await;
            return (make_prompt_json("..."), Err(format!("initialize: {}", e)));
        }
    };
    if let Some(err) = rpc_error(&init_resp) {
        let _ = child.kill().await;
        return (make_prompt_json("..."), Err(format!("initialize failed: {}", err)));
    }

    // ── Step 2: session/new ─────────────────────────────────────────────────
    let session_msg = r#"{"jsonrpc":"2.0","id":"aptove-session","method":"session/new","params":{}}"#;
    if let Err(e) = write_msg(&mut writer, session_msg).await {
        let _ = child.kill().await;
        return (make_prompt_json("..."), Err(e));
    }
    let session_resp = match read_line(&mut lines, 10).await {
        Ok(line) => line,
        Err(e) => {
            let _ = child.kill().await;
            return (make_prompt_json("..."), Err(format!("session/new: {}", e)));
        }
    };
    if let Some(err) = rpc_error(&session_resp) {
        let _ = child.kill().await;
        return (make_prompt_json("..."), Err(format!("session/new failed: {}", err)));
    }
    let session_id = match extract_session_id(&session_resp) {
        Some(id) => id,
        None => {
            let _ = child.kill().await;
            let preview: String = session_resp.chars().take(200).collect();
            return (
                make_prompt_json("..."),
                Err(format!("Could not extract sessionId from: {}", preview)),
            );
        }
    };

    // ── Step 3: session/prompt ──────────────────────────────────────────────
    let prompt_msg = make_prompt_json(&session_id);
    if let Err(e) = write_msg(&mut writer, &prompt_msg).await {
        let _ = child.kill().await;
        return (prompt_msg, Err(e));
    }

    // ── Collect session/update text ─────────────────────────────────────────
    let mut collected = String::new();
    let start = Instant::now();

    loop {
        let elapsed = start.elapsed();
        let remaining = Duration::from_secs(30).checked_sub(elapsed).unwrap_or_default();
        let idle_wait = remaining.min(Duration::from_secs(5));
        if idle_wait.is_zero() { break; }

        match timeout(idle_wait, lines.next_line()).await {
            Err(_) | Ok(Ok(None)) | Ok(Err(_)) => break,
            Ok(Ok(Some(line))) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                    // Check prompt ack for errors
                    if v.get("id").and_then(|i| i.as_str()) == Some("aptove-prompt") {
                        if let Some(err) = v.get("error") {
                            let _ = child.kill().await;
                            return (prompt_msg, Err(format!("session/prompt error: {}", err)));
                        }
                    }
                    if let Some(t) = extract_update_text(&v) {
                        collected.push_str(&t);
                    }
                }
            }
        }
    }

    let _ = child.kill().await;

    if collected.is_empty() {
        (prompt_msg, Err("Agent did not return any text response".to_string()))
    } else {
        (prompt_msg, Ok(collected))
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn make_prompt_json(session_id: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": "aptove-prompt",
        "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": TEST_PROMPT_TEXT}]
        }
    })
    .to_string()
}

async fn write_msg(
    writer: &mut tokio::io::BufWriter<tokio::process::ChildStdin>,
    msg: &str,
) -> Result<(), String> {
    writer
        .write_all(format!("{}\n", msg).as_bytes())
        .await
        .map_err(|e| format!("Write error: {}", e))?;
    writer.flush().await.map_err(|e| format!("Flush error: {}", e))
}

async fn read_line(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    secs: u64,
) -> Result<String, String> {
    timeout(Duration::from_secs(secs), lines.next_line())
        .await
        .map_err(|_| format!("Timeout: no response within {} seconds", secs))?
        .map_err(|e| format!("Read error: {}", e))?
        .ok_or_else(|| "Agent closed stdout without responding".to_string())
}

fn rpc_error(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    v.get("error").map(|e| e.to_string())
}

fn extract_session_id(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    v.get("result")?.get("sessionId")?.as_str().map(str::to_string)
}

/// Extract text from an ACP `session/update` notification (v1 and v2).
fn extract_update_text(v: &serde_json::Value) -> Option<String> {
    if v.get("method")?.as_str()? != "session/update" {
        return None;
    }
    let update = v.get("params")?.get("update")?;
    let kind = update.get("sessionUpdate")?.as_str()?;
    match kind {
        // v1 & v2: single ContentBlock
        "agent_message_chunk" => {
            let c = update.get("content")?;
            if c.get("type")?.as_str()? == "text" {
                c.get("text")?.as_str().map(str::to_string)
            } else {
                None
            }
        }
        // v2: array of ContentBlocks
        "agent_message" => {
            let arr = update.get("content")?.as_array()?;
            let mut s = String::new();
            for block in arr {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                        s.push_str(t);
                    }
                }
            }
            if s.is_empty() { None } else { Some(s) }
        }
        _ => None,
    }
}

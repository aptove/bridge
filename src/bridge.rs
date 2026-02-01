use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response, ErrorResponse};
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::tungstenite::http::StatusCode;
use tracing::{debug, error, info, warn};

/// Bridge between stdio-based ACP agents and WebSocket clients
pub struct StdioBridge {
    agent_command: String,
    port: u16,
    bind_addr: String,
    auth_token: Option<String>,
}

impl StdioBridge {
    pub fn new(agent_command: String, port: u16) -> Self {
        Self {
            agent_command,
            port,
            bind_addr: "0.0.0.0".to_string(),
            auth_token: None,
        }
    }

    /// Set the bind address
    pub fn with_bind_addr(mut self, addr: String) -> Self {
        self.bind_addr = addr;
        self
    }

    /// Set the required authentication token
    pub fn with_auth_token(mut self, token: Option<String>) -> Self {
        self.auth_token = token;
        self
    }

    /// Start the bridge server
    pub async fn start(&self) -> Result<()> {
        let addr = format!("{}:{}", self.bind_addr, self.port);
        let listener = TcpListener::bind(&addr)
            .await
            .context(format!("Failed to bind to {}", addr))?;

        info!("✅ WebSocket server listening on {}", addr);
        if self.auth_token.is_some() {
            info!("🔐 Authentication required for connections");
        } else {
            warn!("⚠️  Authentication disabled - connections are not secured!");
        }
        info!("🤖 Ready to accept mobile connections...");

        let auth_token = Arc::new(self.auth_token.clone());

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    info!("📱 New connection from: {}", addr);
                    let agent_command = self.agent_command.clone();
                    let auth_token = Arc::clone(&auth_token);
                    
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, agent_command, auth_token).await {
                            error!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                }
            }
        }
    }
}

/// Handle a single WebSocket connection
async fn handle_connection(stream: TcpStream, agent_command: String, auth_token: Arc<Option<String>>) -> Result<()> {
    // Custom callback to validate auth token during WebSocket handshake
    let auth_token_for_callback = Arc::clone(&auth_token);
    let callback = move |req: &Request, response: Response| -> std::result::Result<Response, ErrorResponse> {
        if let Some(expected_token) = auth_token_for_callback.as_ref() {
            // Check for auth token in headers
            let token_valid = req.headers()
                .get("X-Bridge-Token")
                .and_then(|v| v.to_str().ok())
                .map(|t| t == expected_token)
                .unwrap_or(false);
            
            // Also check query string as fallback (for clients that can't set headers)
            let query_token_valid = if !token_valid {
                req.uri().query()
                    .and_then(|q| {
                        q.split('&')
                            .find(|p| p.starts_with("token="))
                            .map(|p| &p[6..])
                    })
                    .map(|t| t == expected_token)
                    .unwrap_or(false)
            } else {
                false
            };
            
            if !token_valid && !query_token_valid {
                // Log is not available here since we're in a sync closure
                // Build a 401 error response
                let error_response = tokio_tungstenite::tungstenite::http::Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .body(Some("Unauthorized: invalid or missing auth token".into()))
                    .unwrap();
                return Err(error_response);
            }
        }
        Ok(response)
    };
    
    // Upgrade to WebSocket with auth callback
    let ws_stream = match tokio_tungstenite::accept_hdr_async(stream, callback).await {
        Ok(ws) => ws,
        Err(e) => {
            warn!("🚫 Connection rejected: {}", e);
            return Err(anyhow::anyhow!("WebSocket handshake failed: {}", e));
        }
    };
    
    if auth_token.is_some() {
        info!("🔓 Auth token validated");
    }

    info!("✅ WebSocket connection established");

    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    // Parse the agent command
    let parts: Vec<&str> = agent_command.split_whitespace().collect();
    if parts.is_empty() {
        anyhow::bail!("Empty agent command");
    }

    let command = parts[0];
    let args = &parts[1..];

    // Spawn the ACP agent process
    info!("🚀 Spawning agent: {} {:?}", command, args);
    
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context(format!("Failed to spawn agent command: {}", agent_command))?;

    let stdin = child
        .stdin
        .take()
        .context("Failed to open agent stdin")?;
    
    let stdout = child
        .stdout
        .take()
        .context("Failed to open agent stdout")?;
    
    let stderr = child
        .stderr
        .take()
        .context("Failed to open agent stderr")?;

    // Create channels for coordinating the tasks
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    // Task 1: WebSocket -> Agent stdin
    let mut stdin_writer = stdin;
    let ws_to_agent = tokio::spawn(async move {
        while let Some(msg_result) = ws_receiver.next().await {
            match msg_result {
                Ok(msg) => {
                    if msg.is_text() || msg.is_binary() {
                        let data = msg.into_data();
                        info!("📥 Received from Mobile ({} bytes): {}", data.len(), 
                            String::from_utf8_lossy(&data).chars().take(500).collect::<String>());
                        
                        if let Err(e) = stdin_writer.write_all(&data).await {
                            error!("Failed to write to agent stdin: {}", e);
                            break;
                        }
                        
                        if let Err(e) = stdin_writer.write_all(b"\n").await {
                            error!("Failed to write newline to agent stdin: {}", e);
                            break;
                        }
                        
                        if let Err(e) = stdin_writer.flush().await {
                            error!("Failed to flush agent stdin: {}", e);
                            break;
                        }
                        
                        info!("✅ Forwarded to Copilot agent");
                    } else if msg.is_close() {
                        info!("📱 Client closed connection");
                        break;
                    }
                }
                Err(e) => {
                    error!("WebSocket receive error: {}", e);
                    break;
                }
            }
        }
        
        debug!("WebSocket receiver task ended");
    });

    // Task 2: Agent stdout -> WebSocket
    let shutdown_tx_clone = shutdown_tx.clone();
    let stdout_reader = BufReader::new(stdout);
    let agent_to_ws = tokio::spawn(async move {
        let mut lines = stdout_reader.lines();
        
        while let Ok(Some(line)) = lines.next_line().await {
            info!("📤 Sending to Mobile ({} bytes): {}", line.len(), 
                line.chars().take(500).collect::<String>());
            
            if let Err(e) = ws_sender.send(Message::Text(line)).await {
                error!("Failed to send to WebSocket: {}", e);
                break;
            }
        }
        
        debug!("Agent stdout reader task ended");
        let _ = shutdown_tx_clone.send(()).await;
    });

    // Task 3: Log agent stderr
    let stderr_reader = BufReader::new(stderr);
    let stderr_logger = tokio::spawn(async move {
        let mut lines = stderr_reader.lines();
        
        while let Ok(Some(line)) = lines.next_line().await {
            warn!("🤖 Agent stderr: {}", line);
        }
        
        debug!("Agent stderr reader task ended");
    });

    // Task 4: Monitor child process
    let mut child_monitor = child;
    let shutdown_tx_clone = shutdown_tx.clone();
    let process_monitor = tokio::spawn(async move {
        match child_monitor.wait().await {
            Ok(status) => {
                if status.success() {
                    info!("🤖 Agent process exited successfully");
                } else {
                    error!("🤖 Agent process exited with: {}", status);
                }
            }
            Err(e) => {
                error!("Failed to wait for agent process: {}", e);
            }
        }
        
        let _ = shutdown_tx_clone.send(()).await;
    });

    // Wait for any task to complete (which signals shutdown)
    shutdown_rx.recv().await;
    
    info!("🔌 Connection closing, cleaning up...");

    // Abort all tasks
    ws_to_agent.abort();
    agent_to_ws.abort();
    stderr_logger.abort();
    process_monitor.abort();

    Ok(())
}

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response, ErrorResponse};
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::tungstenite::http::StatusCode;
use tracing::{debug, error, info, warn};

use crate::rate_limiter::RateLimiter;
use crate::tls::TlsConfig;
use crate::pairing::{PairingManager, PairingError, PairingErrorResponse};

/// Bridge between stdio-based ACP agents and WebSocket clients
pub struct StdioBridge {
    agent_command: String,
    port: u16,
    bind_addr: String,
    auth_token: Option<String>,
    rate_limiter: Arc<RateLimiter>,
    tls_config: Option<Arc<TlsConfig>>,
    pairing_manager: Option<Arc<PairingManager>>,
}

impl StdioBridge {
    pub fn new(agent_command: String, port: u16) -> Self {
        Self {
            agent_command,
            port,
            bind_addr: "0.0.0.0".to_string(),
            auth_token: None,
            rate_limiter: Arc::new(RateLimiter::new(3, 10)),
            tls_config: None,
            pairing_manager: None,
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

    /// Set the rate limiter configuration
    pub fn with_rate_limits(mut self, max_connections_per_ip: usize, max_attempts_per_minute: usize) -> Self {
        self.rate_limiter = Arc::new(RateLimiter::new(max_connections_per_ip, max_attempts_per_minute));
        self
    }

    /// Enable TLS with the given configuration
    pub fn with_tls(mut self, tls_config: TlsConfig) -> Self {
        self.tls_config = Some(Arc::new(tls_config));
        self
    }

    /// Enable pairing with the given manager
    pub fn with_pairing(mut self, pairing_manager: PairingManager) -> Self {
        self.pairing_manager = Some(Arc::new(pairing_manager));
        self
    }

    /// Get a reference to the pairing manager (if enabled)
    #[allow(dead_code)]
    pub fn pairing_manager(&self) -> Option<&Arc<PairingManager>> {
        self.pairing_manager.as_ref()
    }

    /// Start the bridge server
    pub async fn start(&self) -> Result<()> {
        let addr = format!("{}:{}", self.bind_addr, self.port);
        let listener = TcpListener::bind(&addr)
            .await
            .context(format!("Failed to bind to {}", addr))?;

        let protocol = if self.tls_config.is_some() { "wss" } else { "ws" };
        info!("✅ WebSocket server listening on {} ({}://{})", addr, protocol, addr);
        
        if self.tls_config.is_some() {
            info!("🔒 TLS enabled");
        } else {
            warn!("⚠️  TLS disabled - connections are not encrypted!");
        }
        
        if self.auth_token.is_some() {
            info!("🔐 Authentication required for connections");
        } else {
            warn!("⚠️  Authentication disabled - connections are not secured!");
        }
        
        if self.pairing_manager.is_some() {
            info!("🔗 Pairing endpoint available at /pair/local");
        }
        
        info!("🤖 Ready to accept mobile connections...");

        let auth_token = Arc::new(self.auth_token.clone());
        let rate_limiter = Arc::clone(&self.rate_limiter);
        let tls_config = self.tls_config.clone();
        let pairing_manager = self.pairing_manager.clone();

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    // Extract IP for rate limiting
                    let client_ip = addr.ip();
                    
                    // Check rate limits before processing
                    if let Err(e) = rate_limiter.check_connection(client_ip).await {
                        warn!("🚫 Rate limit exceeded for {}: {}", client_ip, e);
                        // Connection will be dropped, client should retry later
                        continue;
                    }
                    
                    info!("📱 New connection from: {}", addr);
                    let agent_command = self.agent_command.clone();
                    let auth_token = Arc::clone(&auth_token);
                    let rate_limiter = Arc::clone(&rate_limiter);
                    let tls_config = tls_config.clone();
                    let pairing_manager = pairing_manager.clone();
                    
                    tokio::spawn(async move {
                        // Register connection
                        rate_limiter.add_connection(client_ip).await;
                        
                        let result = if let Some(tls) = tls_config {
                            // TLS connection
                            match tls.acceptor.accept(stream).await {
                                Ok(tls_stream) => {
                                    handle_connection_generic(tls_stream, agent_command, auth_token, pairing_manager).await
                                }
                                Err(e) => {
                                    warn!("🚫 TLS handshake failed: {}", e);
                                    Err(anyhow::anyhow!("TLS handshake failed: {}", e))
                                }
                            }
                        } else {
                            // Plain TCP connection
                            handle_connection_generic(stream, agent_command, auth_token, pairing_manager).await
                        };
                        
                        // Always remove connection when done
                        rate_limiter.remove_connection(client_ip).await;
                        
                        if let Err(e) = result {
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

/// Handle a single connection (generic over stream type for TLS/non-TLS)
/// This function first peeks at the HTTP request to determine if it's:
/// 1. A pairing request (/pair/local) - respond with JSON
/// 2. A WebSocket upgrade request - proceed with WebSocket handling
async fn handle_connection_generic<S>(
    mut stream: S, 
    agent_command: String, 
    auth_token: Arc<Option<String>>,
    pairing_manager: Option<Arc<PairingManager>>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Read the HTTP request headers to determine the request type
    let mut buffer = vec![0u8; 4096];
    let n = stream.read(&mut buffer).await.context("Failed to read request")?;
    let request_data = &buffer[..n];
    
    // Parse the first line to get the path
    let request_str = String::from_utf8_lossy(request_data);
    let first_line = request_str.lines().next().unwrap_or("");
    
    // Check if this is a pairing request
    if first_line.contains("/pair/local") && first_line.starts_with("GET") {
        info!("🔗 Pairing request received");
        return handle_pairing_request(&mut stream, &request_str, pairing_manager).await;
    }
    
    // Otherwise, it's a WebSocket upgrade - we need to create a stream that
    // "unreads" the data we already consumed
    let prefixed_stream = PrefixedStream::new(request_data.to_vec(), stream);
    
    // Continue with WebSocket handling
    handle_websocket_connection(prefixed_stream, agent_command, auth_token).await
}

/// Handle a pairing request - validate the code and return connection details
async fn handle_pairing_request<S>(
    stream: &mut S,
    request: &str,
    pairing_manager: Option<Arc<PairingManager>>,
) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    // Extract the code from the query string
    let code = request
        .lines()
        .next()
        .and_then(|line| {
            // GET /pair/local?code=123456&fp=... HTTP/1.1
            let path_part = line.split_whitespace().nth(1)?;
            let query = path_part.split('?').nth(1)?;
            query
                .split('&')
                .find(|p| p.starts_with("code="))
                .map(|p| p[5..].to_string())
        });

    let Some(code) = code else {
        let response = create_http_response(400, "Bad Request", r#"{"error":"missing_code","message":"Missing 'code' query parameter"}"#);
        stream.write_all(response.as_bytes()).await?;
        return Ok(());
    };

    let Some(manager) = pairing_manager else {
        let response = create_http_response(503, "Service Unavailable", r#"{"error":"pairing_disabled","message":"Pairing is not enabled on this bridge"}"#);
        stream.write_all(response.as_bytes()).await?;
        return Ok(());
    };

    // Validate the pairing code
    match manager.validate(&code) {
        Ok(pairing_response) => {
            info!("✅ Pairing successful");
            let json = serde_json::to_string(&pairing_response).unwrap_or_default();
            let response = create_http_response(200, "OK", &json);
            stream.write_all(response.as_bytes()).await?;
        }
        Err(PairingError::RateLimited) => {
            warn!("🚫 Pairing rate limited");
            let json = serde_json::to_string(&PairingErrorResponse::rate_limited()).unwrap_or_default();
            let response = create_http_response(429, "Too Many Requests", &json);
            stream.write_all(response.as_bytes()).await?;
        }
        Err(_) => {
            warn!("🚫 Invalid pairing code");
            let json = serde_json::to_string(&PairingErrorResponse::invalid_code()).unwrap_or_default();
            let response = create_http_response(401, "Unauthorized", &json);
            stream.write_all(response.as_bytes()).await?;
        }
    }

    Ok(())
}

/// Create an HTTP response with the given status and body
fn create_http_response(status_code: u16, status_text: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {} {}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        status_code,
        status_text,
        body.len(),
        body
    )
}

/// A stream wrapper that prepends buffered data before reading from the underlying stream
struct PrefixedStream<S> {
    prefix: Vec<u8>,
    prefix_pos: usize,
    inner: S,
}

impl<S> PrefixedStream<S> {
    fn new(prefix: Vec<u8>, inner: S) -> Self {
        Self {
            prefix,
            prefix_pos: 0,
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedStream<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        // First, drain the prefix buffer
        if self.prefix_pos < self.prefix.len() {
            let remaining = &self.prefix[self.prefix_pos..];
            let to_copy = std::cmp::min(remaining.len(), buf.remaining());
            buf.put_slice(&remaining[..to_copy]);
            self.prefix_pos += to_copy;
            return std::task::Poll::Ready(Ok(()));
        }
        
        // Then read from the inner stream
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedStream<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Handle WebSocket connection after initial HTTP parsing
async fn handle_websocket_connection<S>(stream: S, agent_command: String, auth_token: Arc<Option<String>>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
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
                        debug!("📥 Received from Mobile ({} bytes): {}", data.len(), 
                            String::from_utf8_lossy(&data).chars().take(200).collect::<String>());
                        
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
                        
                        debug!("✅ Forwarded to agent");
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
            debug!("📤 Sending to Mobile ({} bytes): {}", line.len(), 
                line.chars().take(200).collect::<String>());
            
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

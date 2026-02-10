use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response, ErrorResponse};
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::tungstenite::http::StatusCode;
use tracing::{debug, error, info, warn};

use crate::agent_pool::AgentPool;
use crate::rate_limiter::RateLimiter;
use crate::tls::TlsConfig;
use crate::pairing::{PairingManager, PairingError, PairingErrorResponse};
use crate::push::PushRelayClient;

/// Bridge between stdio-based ACP agents and WebSocket clients
pub struct StdioBridge {
    agent_command: String,
    port: u16,
    bind_addr: String,
    auth_token: Option<String>,
    rate_limiter: Arc<RateLimiter>,
    tls_config: Option<Arc<TlsConfig>>,
    pairing_manager: Option<Arc<PairingManager>>,
    agent_pool: Option<Arc<tokio::sync::RwLock<AgentPool>>>,
    push_relay: Option<Arc<PushRelayClient>>,
}

impl StdioBridge {
    pub fn new(agent_command: String, port: u16) -> Self {
        Self {
            agent_command,
            port,
            bind_addr: "0.0.0.0".to_string(),
            auth_token: None,
            rate_limiter: Arc::new(RateLimiter::new(10, 30)),
            tls_config: None,
            pairing_manager: None,
            agent_pool: None,
            push_relay: None,
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

    /// Enable agent pool for keep-alive sessions
    pub fn with_agent_pool(mut self, pool: Arc<tokio::sync::RwLock<AgentPool>>) -> Self {
        self.agent_pool = Some(pool);
        self
    }

    /// Enable push notifications via relay
    pub fn with_push_relay(mut self, client: PushRelayClient) -> Self {
        self.push_relay = Some(Arc::new(client));
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
                    let agent_pool = self.agent_pool.clone();
                    let push_relay = self.push_relay.clone();
                    
                    tokio::spawn(async move {
                        // Register connection
                        rate_limiter.add_connection(client_ip).await;
                        
                        let result = if let Some(tls) = tls_config {
                            // TLS connection
                            match tls.acceptor.accept(stream).await {
                                Ok(tls_stream) => {
                                    handle_connection_generic(tls_stream, agent_command, auth_token, pairing_manager, agent_pool, push_relay).await
                                }
                                Err(e) => {
                                    warn!("🚫 TLS handshake failed: {}", e);
                                    Err(anyhow::anyhow!("TLS handshake failed: {}", e))
                                }
                            }
                        } else {
                            // Plain TCP connection
                            handle_connection_generic(stream, agent_command, auth_token, pairing_manager, agent_pool, push_relay).await
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
    agent_pool: Option<Arc<tokio::sync::RwLock<AgentPool>>>,
    push_relay: Option<Arc<PushRelayClient>>,
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
    handle_websocket_connection(prefixed_stream, agent_command, auth_token, agent_pool, push_relay).await
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
async fn handle_websocket_connection<S>(stream: S, agent_command: String, auth_token: Arc<Option<String>>, agent_pool: Option<Arc<tokio::sync::RwLock<AgentPool>>>, push_relay: Option<Arc<PushRelayClient>>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Custom callback to validate auth token during WebSocket handshake
    // We also extract the token value for pool-based routing
    let auth_token_for_callback = Arc::clone(&auth_token);
    let extracted_token = Arc::new(tokio::sync::Mutex::new(String::new()));
    let extracted_token_clone = Arc::clone(&extracted_token);
    
    let callback = move |req: &Request, response: Response| -> std::result::Result<Response, ErrorResponse> {
        if let Some(expected_token) = auth_token_for_callback.as_ref() {
            // Check for auth token in headers
            let header_token = req.headers()
                .get("X-Bridge-Token")
                .and_then(|v| v.to_str().ok())
                .map(|t| t.to_string());
            
            let token_valid = header_token.as_deref()
                .map(|t| t == expected_token)
                .unwrap_or(false);
            
            // Also check query string as fallback
            let query_token = if !token_valid {
                req.uri().query()
                    .and_then(|q| {
                        q.split('&')
                            .find(|p| p.starts_with("token="))
                            .map(|p| p[6..].to_string())
                    })
            } else {
                None
            };
            
            let query_token_valid = query_token.as_deref()
                .map(|t| t == expected_token)
                .unwrap_or(false);
            
            if !token_valid && !query_token_valid {
                let error_response = tokio_tungstenite::tungstenite::http::Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .body(Some("Unauthorized: invalid or missing auth token".into()))
                    .unwrap();
                return Err(error_response);
            }
            
            // Store the validated token for pool routing
            if let Some(t) = header_token.filter(|t| t == expected_token).or(query_token.filter(|t| t == expected_token)) {
                // We can't await here (sync closure), so use try_lock
                if let Ok(mut guard) = extracted_token_clone.try_lock() {
                    *guard = t;
                }
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

    // Get the token value for pool routing
    let client_token = extracted_token.lock().await.clone();
    
    // Decide whether to use pool-based or legacy handling
    if let Some(pool) = agent_pool {
        if client_token.is_empty() {
            warn!("Keep-alive enabled but no auth token found, falling back to legacy mode");
            handle_websocket_legacy(ws_stream, agent_command, push_relay).await
        } else {
            handle_websocket_pooled(ws_stream, agent_command, client_token, pool, push_relay).await
        }
    } else {
        handle_websocket_legacy(ws_stream, agent_command, push_relay).await
    }
}

/// Handle WebSocket connection with agent pool (keep-alive mode)
async fn handle_websocket_pooled<S>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    agent_command: String,
    token: String,
    pool: Arc<tokio::sync::RwLock<AgentPool>>,
    push_relay: Option<Arc<PushRelayClient>>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    
    // Get or spawn agent from pool
    let (ws_to_agent_tx, mut agent_to_ws_rx, buffered, was_reused, cached_init, cached_session) = {
        let mut pool = pool.write().await;
        pool.get_or_spawn(&token, &agent_command).await?
    };
    
    if was_reused {
        info!("♻️  Reconnected to existing agent session");
    } else {
        info!("🆕 Started new agent session");
    }
    
    // Replay buffered messages
    for msg in buffered {
        debug!("📦 Replaying buffered message: {}", msg.chars().take(200).collect::<String>());
        if let Err(e) = ws_sender.send(Message::Text(msg)).await {
            error!("Failed to replay buffered message: {}", e);
        }
    }
    
    // If reconnecting and we have a cached initialize response, intercept the
    // client's `initialize` request and reply with the cached response.
    // This prevents the agent from being re-initialized and losing its state.
    if was_reused {
        if let Some(ref cached) = cached_init {
            info!("🔄 Intercepting initialize for session resumption");
            // Wait for the client's first message (should be `initialize`)
            let init_handled = handle_initialize_intercept(
                &mut ws_receiver, &mut ws_sender, cached
            ).await;
            if init_handled {
                info!("✅ Initialize intercepted, session state preserved");
            } else {
                warn!("⚠️  First message was not initialize, proceeding normally");
            }
        } else {
            debug!("No cached initialize response, first connection will capture it");
        }
        
        // Also intercept session requests (session/new or session/load) to reuse the same session ID
        if let Some(ref cached) = cached_session {
            info!("🔄 Intercepting session request for session resumption");
            let session_handled = handle_create_session_intercept(
                &mut ws_receiver, &mut ws_sender, cached
            ).await;
            if session_handled {
                info!("✅ Session request intercepted, reusing existing session");
            } else {
                warn!("⚠️  Next message was not a session request, proceeding normally");
            }
        } else {
            debug!("No cached session response, first connection will capture it");
        }
    }
    
    // Create shutdown channel
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
    
    // For a fresh connection, we need to capture the initialize response
    // from the agent so we can cache it for future reconnections.
    let needs_init_capture = !was_reused;
    let token_for_capture = token.clone();
    let pool_for_capture = Arc::clone(&pool);
    
    // Task 1: WebSocket → Agent (via channel)
    let ws_to_agent_tx_clone = ws_to_agent_tx.clone();
    let push_relay_for_register = push_relay.clone();
    let mut ws_to_agent = tokio::spawn(async move {
        while let Some(msg_result) = ws_receiver.next().await {
            match msg_result {
                Ok(msg) => {
                    if msg.is_text() || msg.is_binary() {
                        let data = msg.into_data();
                        let text = String::from_utf8_lossy(&data).to_string();
                        debug!("📥 Received from Mobile ({} bytes): {}", data.len(), 
                            text.chars().take(200).collect::<String>());
                        
                        // Intercept bridge/registerPushToken notifications
                        if let Some(ref relay) = push_relay_for_register {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                                let method = v.get("method").and_then(|m| m.as_str());
                                if method == Some("bridge/registerPushToken") {
                                    if let Some(params) = v.get("params") {
                                        let platform = params.get("platform").and_then(|p| p.as_str()).unwrap_or("");
                                        let device_token = params.get("deviceToken").and_then(|t| t.as_str()).unwrap_or("");
                                        let bundle_id = params.get("bundleId").and_then(|b| b.as_str()).unwrap_or("");
                                        info!("📲 Registering push token: platform={}, bundle_id={}", platform, bundle_id);
                                        let relay = Arc::clone(relay);
                                        let platform = platform.to_string();
                                        let device_token = device_token.to_string();
                                        let bundle_id = bundle_id.to_string();
                                        tokio::spawn(async move {
                                            if let Err(e) = relay.register_device(&platform, &device_token, Some(&bundle_id)).await {
                                                error!("Failed to register push token: {}", e);
                                            } else {
                                                info!("✅ Push token registered successfully");
                                            }
                                        });
                                    }
                                    // Don't forward bridge-specific messages to agent
                                    continue;
                                }
                                // Also handle unregisterPushToken
                                if method == Some("bridge/unregisterPushToken") {
                                    if let Some(params) = v.get("params") {
                                        let device_token = params.get("deviceToken").and_then(|t| t.as_str()).unwrap_or("");
                                        info!("📲 Unregistering push token");
                                        let relay = Arc::clone(relay);
                                        let device_token = device_token.to_string();
                                        tokio::spawn(async move {
                                            if let Err(e) = relay.unregister_device(&device_token).await {
                                                error!("Failed to unregister push token: {}", e);
                                            }
                                        });
                                    }
                                    continue;
                                }
                            }
                        }
                        
                        if ws_to_agent_tx_clone.send(text).await.is_err() {
                            error!("Failed to send to agent channel");
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
    
    // Task 2: Agent → WebSocket (via broadcast channel)
    let shutdown_tx_clone = shutdown_tx.clone();
    let token_for_buffer = token.clone();
    let pool_for_buffer = Arc::clone(&pool);
    let agent_to_ws = tokio::spawn(async move {
        let mut init_captured = false;
        let mut session_captured = false;
        loop {
            match agent_to_ws_rx.recv().await {
                Ok(line) => {
                    // On first connection, capture the initialize response
                    if needs_init_capture && !init_captured {
                        if is_initialize_response(&line) {
                            info!("📋 Captured initialize response for future reconnections");
                            let mut pool = pool_for_capture.write().await;
                            pool.cache_init_response(&token_for_capture, line.clone());
                            init_captured = true;
                        }
                    }
                    
                    // On first connection, capture the createSession response
                    if needs_init_capture && !session_captured {
                        if is_create_session_response(&line) {
                            info!("📋 Captured createSession response for future reconnections");
                            let mut pool = pool_for_capture.write().await;
                            pool.cache_session_response(&token_for_capture, line.clone());
                            session_captured = true;
                        }
                    }
                    
                    debug!("📤 Sending to Mobile ({} bytes): {}", line.len(), 
                        line.chars().take(200).collect::<String>());
                    
                    if let Err(e) = ws_sender.send(Message::Text(line.clone())).await {
                        debug!("Client disconnected, buffering message: {}", e);
                        let mut pool = pool_for_buffer.write().await;
                        pool.buffer_message(&token_for_buffer, line);
                        // Send push notification since client is disconnected
                        if let Some(ref relay) = push_relay {
                            let relay = Arc::clone(relay);
                            tokio::spawn(async move {
                                if let Err(e) = relay.notify("Agent").await {
                                    debug!("Push notification failed: {}", e);
                                }
                            });
                        }
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Agent-to-WS receiver lagged, skipped {} messages", n);
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    debug!("Agent broadcast channel closed (agent exited)");
                    break;
                }
            }
        }
        
        debug!("Agent-to-WS forwarder task ended");
        let _ = shutdown_tx_clone.send(()).await;
    });
    
    // Wait for either task to finish
    tokio::select! {
        _ = &mut ws_to_agent => {
            debug!("WS-to-agent task completed first");
        }
        _ = shutdown_rx.recv() => {
            debug!("Agent-to-WS task completed first");
        }
    }
    
    info!("💤 Client disconnected, agent stays alive in pool");
    
    // Abort forwarding tasks - agent process stays alive
    ws_to_agent.abort();
    agent_to_ws.abort();
    
    // Mark agent as disconnected in pool (don't kill it)
    {
        let mut pool = pool.write().await;
        pool.mark_disconnected(&token);
    }
    
    Ok(())
}

/// Check if a JSON-RPC message is an `initialize` response.
/// Supports both MCP-style (capabilities, serverInfo) and ACP-style (agentCapabilities, agentInfo, protocolVersion) responses.
fn is_initialize_response(msg: &str) -> bool {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(msg) {
        // It's a response (has "result") and the result contains agent/server capabilities
        v.get("result").is_some()
            && (v["result"].get("capabilities").is_some()
                || v["result"].get("serverInfo").is_some()
                || v["result"].get("agentInfo").is_some()
                || v["result"].get("agentCapabilities").is_some()
                || v["result"].get("protocolVersion").is_some())
    } else {
        false
    }
}

/// Check if a JSON-RPC message is a `createSession` response (has "result" with "sessionId")
fn is_create_session_response(msg: &str) -> bool {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(msg) {
        // It's a response (has "result") and the result contains a sessionId
        if let Some(result) = v.get("result") {
            result.get("sessionId").is_some()
        } else {
            false
        }
    } else {
        false
    }
}

/// Intercept the client's `createSession` request and reply with a cached response.
/// Returns true if a createSession was intercepted, false otherwise.
async fn handle_create_session_intercept<S>(
    ws_receiver: &mut futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
    ws_sender: &mut futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, Message>,
    cached_response: &str,
) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // The ACP protocol flow after initialize is:
    //   Client → notifications/initialized (notification, no id)
    //   Client → session/new (request) OR session/load (request for reconnection)
    // We need to skip any notifications before finding the session request.
    let mut request: serde_json::Value;
    let max_skip = 5; // safety limit to avoid infinite loop
    let mut skipped = 0;
    
    loop {
        let msg = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            ws_receiver.next(),
        ).await {
            Ok(Some(Ok(msg))) if msg.is_text() || msg.is_binary() => {
                String::from_utf8_lossy(&msg.into_data()).to_string()
            }
            _ => return false,
        };
        
        request = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(_) => return false,
        };
        
        let method = request.get("method").and_then(|m| m.as_str());
        
        // Accept both session/new (new session) and session/load (resume session)
        if method == Some("session/new") || method == Some("session/load") {
            break; // found it
        }
        
        // If it's a notification (has method but no id), skip it
        if method.is_some() && request.get("id").is_none() {
            info!("📨 Skipping notification during session intercept: {:?}", method);
            skipped += 1;
            if skipped >= max_skip {
                warn!("⚠️  Too many notifications before session request, giving up");
                return false;
            }
            continue;
        }
        
        // If it's an initialize request, respond with a minimal ACP initialize response
        // and continue looking for session/new. This happens when cached_init was None
        // (e.g., agent's initialize response format wasn't recognized on first connection).
        if method == Some("initialize") {
            if let Some(req_id) = request.get("id") {
                info!("📨 Handling uncached initialize during session intercept (id={})", req_id);
                let init_response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "protocolVersion": 1,
                        "agentCapabilities": {},
                        "agentInfo": {
                            "name": "bridge",
                            "version": "1.0.0"
                        }
                    }
                });
                let resp_str = serde_json::to_string(&init_response).unwrap_or_default();
                if let Err(e) = ws_sender.send(Message::Text(resp_str)).await {
                    error!("Failed to send synthetic initialize response: {}", e);
                    return false;
                }
                skipped += 1;
                if skipped >= max_skip {
                    warn!("⚠️  Too many messages before session request, giving up");
                    return false;
                }
                continue;
            }
        }
        
        // It's some other request, not a session request — can't intercept
        warn!("⚠️  Message is not session/new or session/load (method={:?}, has_id={}, raw={}), cannot intercept", 
            method, request.get("id").is_some(), 
            msg.chars().take(200).collect::<String>());
        return false;
    }
    
    // Extract the request ID so we can match it in the response
    let request_id = match request.get("id") {
        Some(id) => id.clone(),
        None => return false,
    };
    
    let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("unknown");
    info!("🔄 Intercepting {} request (id={})", method, request_id);
    
    // Parse the cached response and replace its "id" with the new request's "id"
    let mut cached: serde_json::Value = match serde_json::from_str(cached_response) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to parse cached session response: {}", e);
            return false;
        }
    };
    
    cached["id"] = request_id;
    
    let response_str = serde_json::to_string(&cached).unwrap_or_default();
    debug!("🔄 Sending cached session response ({} bytes): {}", response_str.len(),
        response_str.chars().take(200).collect::<String>());
    
    if let Err(e) = ws_sender.send(Message::Text(response_str)).await {
        error!("Failed to send cached session response: {}", e);
        return false;
    }
    
    true
}

/// Intercept the client's `initialize` request and reply with a cached response.
/// Returns true if an initialize was intercepted, false otherwise.
async fn handle_initialize_intercept<S>(
    ws_receiver: &mut futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
    ws_sender: &mut futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, Message>,
    cached_response: &str,
) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // Read the first message from the client
    let first_msg = match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        ws_receiver.next(),
    ).await {
        Ok(Some(Ok(msg))) if msg.is_text() || msg.is_binary() => {
            String::from_utf8_lossy(&msg.into_data()).to_string()
        }
        _ => return false,
    };
    
    // Parse it as JSON-RPC to check if it's an `initialize` request
    let request: serde_json::Value = match serde_json::from_str(&first_msg) {
        Ok(v) => v,
        Err(_) => return false,
    };
    
    let method = request.get("method").and_then(|m| m.as_str());
    if method != Some("initialize") {
        debug!("First message is not initialize (method={:?}), cannot intercept", method);
        return false;
    }
    
    // Extract the request ID so we can match it in the response
    let request_id = match request.get("id") {
        Some(id) => id.clone(),
        None => return false,
    };
    
    info!("🔄 Intercepting initialize request (id={})", request_id);
    
    // Parse the cached response and replace its "id" with the new request's "id"
    let mut cached: serde_json::Value = match serde_json::from_str(cached_response) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to parse cached initialize response: {}", e);
            return false;
        }
    };
    
    cached["id"] = request_id;
    
    let response_str = serde_json::to_string(&cached).unwrap_or_default();
    debug!("🔄 Sending cached initialize response ({} bytes)", response_str.len());
    
    if let Err(e) = ws_sender.send(Message::Text(response_str)).await {
        error!("Failed to send cached initialize response: {}", e);
        return false;
    }
    
    true
}


/// Handle WebSocket connection in legacy mode (kill-on-drop, no pool)
async fn handle_websocket_legacy<S>(ws_stream: tokio_tungstenite::WebSocketStream<S>, agent_command: String, _push_relay: Option<Arc<PushRelayClient>>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
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

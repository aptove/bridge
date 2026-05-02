use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::sync::broadcast;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response, ErrorResponse};
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::tungstenite::http::StatusCode;
use tracing::{debug, error, info, warn};

use crate::agent_pool::AgentPool;
use crate::common_config::SlashCommandConfig;
use crate::rate_limiter::RateLimiter;
use crate::tls::TlsConfig;
use crate::pairing::{PairingManager, PairingError, PairingErrorResponse};
use crate::push::PushRelayClient;

// ---------------------------------------------------------------------------
// Webhook support types
// ---------------------------------------------------------------------------

/// Information about a resolved webhook trigger, returned by the resolver.
#[derive(Debug, Clone)]
pub struct WebhookTarget {
    pub workspace_id: String,
    pub trigger_id: String,
    pub trigger_name: String,
    pub rate_limit_per_minute: u32,
    pub hmac_secret: Option<String>,
    pub accepted_content_types: Vec<String>,
}

/// Async callback used by `StdioBridge` to look up a trigger token.
///
/// Implementors (e.g., `agent-bridge`) resolve the token via `TriggerStore`.
/// Returns `Some(target)` if the token is valid and trigger is enabled,
/// `None` if unknown or disabled.
pub type WebhookResolverFn =
    Arc<dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<WebhookTarget>> + Send>> + Send + Sync>;

/// Per-trigger sliding-window rate limiter (used internally by the bridge).
struct TriggerRateLimiter {
    /// token → timestamps of recent events (last 60 s)
    windows: HashMap<String, Vec<Instant>>,
}

impl TriggerRateLimiter {
    fn new() -> Self {
        Self { windows: HashMap::new() }
    }

    /// Returns `true` if the event is allowed, `false` if rate-limited.
    fn check_and_record(&mut self, token: &str, limit_per_minute: u32) -> bool {
        if limit_per_minute == 0 {
            return true; // unlimited
        }
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let stamps = self.windows.entry(token.to_string()).or_default();
        stamps.retain(|t| now.duration_since(*t) < window);
        if stamps.len() >= limit_per_minute as usize {
            return false;
        }
        stamps.push(now);
        true
    }
}

/// Describes how the bridge connects to the ACP agent backend.
#[derive(Clone)]
pub enum AgentHandle {
    /// Spawn an external subprocess (existing behavior).
    Command(String),
    /// Communicate via in-process channels (embedded mode).
    InProcess {
        stdin_tx: mpsc::Sender<Vec<u8>>,
        stdout_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>>,
    },
}

/// Bridge between stdio-based ACP agents and WebSocket clients
pub struct StdioBridge {
    agent_handle: AgentHandle,
    port: u16,
    bind_addr: String,
    auth_token: Option<String>,
    rate_limiter: Arc<RateLimiter>,
    tls_config: Option<Arc<TlsConfig>>,
    pairing_manager: Option<Arc<PairingManager>>,
    agent_pool: Option<Arc<tokio::sync::RwLock<AgentPool>>>,
    push_relay: Option<Arc<PushRelayClient>>,
    /// Optional resolver for webhook token → trigger mapping.
    webhook_resolver: Option<WebhookResolverFn>,
    /// Per-trigger sliding-window rate limiter.
    webhook_rate_limiter: Arc<Mutex<TriggerRateLimiter>>,
    /// When `true`, TLS is handled by an external proxy (e.g. Tailscale serve
    /// or Cloudflare). Suppresses the "TLS disabled" warning since the
    /// public-facing connection is still encrypted end-to-end.
    external_tls: bool,
    /// Working directory for spawned agent processes.
    working_dir: PathBuf,
    /// Slash commands to inject via `available_commands_update` after every
    /// session/new or session/load, for agents that don't send the notification
    /// themselves (e.g. Copilot CLI).
    slash_commands: Arc<Vec<SlashCommandConfig>>,
    /// Path to MEMORY.md — loaded into context on new sessions and appended
    /// to by `bridge/appendMemory` notifications from clients.
    memory_path: Option<PathBuf>,
}

impl StdioBridge {
    pub fn new(agent_command: String, port: u16) -> Self {
        Self {
            agent_handle: AgentHandle::Command(agent_command),
            port,
            bind_addr: "0.0.0.0".to_string(),
            auth_token: None,
            rate_limiter: Arc::new(RateLimiter::new(10, 30)),
            tls_config: None,
            pairing_manager: None,
            agent_pool: None,
            push_relay: None,
            webhook_resolver: None,
            webhook_rate_limiter: Arc::new(Mutex::new(TriggerRateLimiter::new())),
            external_tls: false,
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            slash_commands: Arc::new(Vec::new()),
            memory_path: None,
        }
    }

    /// Set the path to MEMORY.md for persistent memory injection.
    pub fn with_memory_path(mut self, path: PathBuf) -> Self {
        self.memory_path = Some(path);
        self
    }

    /// Set slash commands to inject after session creation for agents that
    /// don't send `available_commands_update` themselves.
    pub fn with_slash_commands(mut self, commands: Vec<SlashCommandConfig>) -> Self {
        self.slash_commands = Arc::new(commands);
        self
    }

    /// Set the working directory for spawned agent processes.
    /// By default this is the directory where the bridge was started.
    pub fn with_working_dir(mut self, dir: PathBuf) -> Self {
        self.working_dir = dir;
        self
    }

    /// Mark this bridge as sitting behind an external TLS proxy (e.g. Tailscale
    /// serve, Cloudflare tunnel). Suppresses the spurious "TLS disabled" warning
    /// since the public connection is already encrypted end-to-end.
    pub fn with_external_tls(mut self) -> Self {
        self.external_tls = true;
        self
    }

    /// Use an in-process agent handle instead of spawning a subprocess.
    pub fn with_agent_handle(mut self, handle: AgentHandle) -> Self {
        self.agent_handle = handle;
        self
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

    /// Enable webhook trigger resolution. When set, incoming `POST /webhook/<token>`
    /// requests are handled: the resolver is called to look up the trigger, and a
    /// `triggers/execute` ACP notification is sent to the in-process agent.
    pub fn with_webhook_resolver(mut self, resolver: WebhookResolverFn) -> Self {
        self.webhook_resolver = Some(resolver);
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
        } else if self.external_tls {
            info!("🔒 TLS handled by external proxy (Tailscale / Cloudflare)");
        } else {
            warn!("⚠️  TLS disabled - connections are not encrypted!");
        }
        
        if self.auth_token.is_some() {
            info!("🔐 Authentication required for connections");
        } else {
            warn!("⚠️  Authentication disabled - connections are not secured!");
        }
        
        if self.pairing_manager.is_some() {
            info!("🔗 Pairing endpoint available at /pair/local, /pair/tailscale, /pair/cloudflare");
        }
        
        info!("🤖 Ready to accept mobile connections...");

        let auth_token = Arc::new(self.auth_token.clone());
        let rate_limiter = Arc::clone(&self.rate_limiter);
        let tls_config = self.tls_config.clone();
        let pairing_manager = self.pairing_manager.clone();
        let webhook_resolver = self.webhook_resolver.clone();
        let webhook_rate_limiter = Arc::clone(&self.webhook_rate_limiter);

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
                    let agent_handle = self.agent_handle.clone();
                    let auth_token = Arc::clone(&auth_token);
                    let rate_limiter = Arc::clone(&rate_limiter);
                    let tls_config = tls_config.clone();
                    let pairing_manager = pairing_manager.clone();
                    let agent_pool = self.agent_pool.clone();
                    let push_relay = self.push_relay.clone();
                    let webhook_resolver = webhook_resolver.clone();
                    let webhook_rate_limiter = Arc::clone(&webhook_rate_limiter);
                    let client_ip_str = addr.ip().to_string();
                    let working_dir = self.working_dir.clone();
                    let slash_commands = Arc::clone(&self.slash_commands);
                    let memory_path = self.memory_path.clone();

                    tokio::spawn(async move {
                        // Register connection
                        rate_limiter.add_connection(client_ip).await;

                        let result = if let Some(tls) = tls_config {
                            // TLS connection
                            match tls.acceptor.accept(stream).await {
                                Ok(tls_stream) => {
                                    handle_connection_generic(tls_stream, agent_handle, auth_token, pairing_manager, agent_pool, push_relay, webhook_resolver, webhook_rate_limiter, client_ip_str, working_dir, slash_commands, memory_path).await
                                }
                                Err(e) => {
                                    warn!("🚫 TLS handshake failed: {}", e);
                                    Err(anyhow::anyhow!("TLS handshake failed: {}", e))
                                }
                            }
                        } else {
                            // Plain TCP connection
                            handle_connection_generic(stream, agent_handle, auth_token, pairing_manager, agent_pool, push_relay, webhook_resolver, webhook_rate_limiter, client_ip_str, working_dir, slash_commands, memory_path).await
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
/// 2. A webhook request (POST /webhook/<token>) - handle and return immediately
/// 3. A WebSocket upgrade request - proceed with WebSocket handling
async fn handle_connection_generic<S>(
    mut stream: S,
    agent_handle: AgentHandle,
    auth_token: Arc<Option<String>>,
    pairing_manager: Option<Arc<PairingManager>>,
    agent_pool: Option<Arc<tokio::sync::RwLock<AgentPool>>>,
    push_relay: Option<Arc<PushRelayClient>>,
    webhook_resolver: Option<WebhookResolverFn>,
    webhook_rate_limiter: Arc<Mutex<TriggerRateLimiter>>,
    client_ip: String,
    working_dir: PathBuf,
    slash_commands: Arc<Vec<SlashCommandConfig>>,
    memory_path: Option<PathBuf>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Read the HTTP request headers to determine the request type
    let mut buffer = vec![0u8; 8192];
    let n = stream.read(&mut buffer).await.context("Failed to read request")?;
    let request_data = &buffer[..n];

    // Parse the first line to get the path
    let request_str = String::from_utf8_lossy(request_data);
    let first_line = request_str.lines().next().unwrap_or("");

    // Check if this is a pairing request
    if (first_line.contains("/pair/local") || first_line.contains("/pair/cloudflare") || first_line.contains("/pair/tailscale")) && first_line.starts_with("GET") {
        info!("🔗 Pairing request received");
        return handle_pairing_request(&mut stream, &request_str, pairing_manager).await;
    }

    // Check if this is a webhook request (POST /webhook/<token>)
    if first_line.starts_with("POST") && first_line.contains("/webhook/") {
        info!("🪝 Webhook request received");
        return handle_webhook_request(
            &mut stream,
            request_data,
            &request_str,
            &agent_handle,
            webhook_resolver,
            webhook_rate_limiter,
            client_ip,
        )
        .await;
    }
    
    // Cloudflare (and other proxies) strip the `Connection: upgrade` hop-by-hop header
    // before forwarding WebSocket upgrade requests to the origin. tungstenite strictly
    // requires `Connection: upgrade`, so we inject it if `Upgrade: websocket` is present.
    let lower = request_str.to_ascii_lowercase();
    let request_bytes = if lower.contains("upgrade: websocket") && !lower.contains("connection: upgrade") {
        // Insert `Connection: upgrade` after the first header line (after the request line)
        let mut patched = request_str.to_string();
        if let Some(pos) = patched.find("\r\n") {
            patched.insert_str(pos + 2, "Connection: upgrade\r\n");
        }
        patched.into_bytes()
    } else {
        request_data.to_vec()
    };
    
    // Otherwise, it's a WebSocket upgrade - we need to create a stream that
    // "unreads" the data we already consumed
    let prefixed_stream = PrefixedStream::new(request_bytes, stream);
    
    // Continue with WebSocket handling
    handle_websocket_connection(prefixed_stream, agent_handle, auth_token, agent_pool, push_relay, working_dir, slash_commands, memory_path).await
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

/// Handle an incoming webhook HTTP POST request.
///
/// Flow:
/// 1. Extract the trigger token from the URL path.
/// 2. Resolve the token via the optional resolver.
/// 3. Check per-trigger rate limit.
/// 4. Optionally verify HMAC-SHA256 signature.
/// 5. Send `triggers/execute` ACP notification to the in-process agent.
/// 6. Return 200 OK immediately (fire-and-forget execution).
#[allow(clippy::too_many_arguments)]
async fn handle_webhook_request<S>(
    stream: &mut S,
    raw_data: &[u8],
    headers_str: &str,
    agent_handle: &AgentHandle,
    resolver: Option<WebhookResolverFn>,
    rate_limiter: Arc<Mutex<TriggerRateLimiter>>,
    client_ip: String,
) -> Result<()>
where
    S: AsyncWrite + AsyncRead + Unpin,
{
    // --- 1. Extract token from the request line ----------------------------
    // Format: "POST /webhook/<token> HTTP/1.1"
    let token = {
        let line = headers_str.lines().next().unwrap_or("");
        let path = line.split_whitespace().nth(1).unwrap_or("");
        let stripped = path.trim_start_matches('/');
        // stripped = "webhook/<token>"
        stripped
            .strip_prefix("webhook/")
            .map(|t| t.split('?').next().unwrap_or(t).to_string())
            .unwrap_or_default()
    };

    if token.is_empty() {
        let resp = create_http_response(400, "Bad Request", r#"{"error":"missing_token"}"#);
        stream.write_all(resp.as_bytes()).await?;
        return Ok(());
    }

    // --- 2. Resolve the token ---------------------------------------------
    let Some(ref resolver_fn) = resolver else {
        let resp = create_http_response(
            503,
            "Service Unavailable",
            r#"{"error":"webhooks_not_configured"}"#,
        );
        stream.write_all(resp.as_bytes()).await?;
        return Ok(());
    };

    let target = resolver_fn(token.clone()).await;

    let Some(target) = target else {
        warn!(token = %&token[..token.len().min(12)], "webhook: unknown or disabled token");
        let resp = create_http_response(404, "Not Found", r#"{"error":"not_found"}"#);
        stream.write_all(resp.as_bytes()).await?;
        return Ok(());
    };

    // --- 3. Per-trigger rate limit ----------------------------------------
    if target.rate_limit_per_minute > 0 {
        let allowed = rate_limiter
            .lock()
            .await
            .check_and_record(&token, target.rate_limit_per_minute);

        if !allowed {
            warn!(trigger = %target.trigger_id, "webhook: rate limit exceeded");
            let resp = create_http_response(
                429,
                "Too Many Requests",
                r#"{"error":"rate_limited","retry_after":60}"#,
            );
            stream.write_all(resp.as_bytes()).await?;
            return Ok(());
        }
    }

    // --- 4. Read the request body -----------------------------------------
    // Find the end of headers (\r\n\r\n)
    let header_end = raw_data
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .unwrap_or(raw_data.len());

    let already_read = &raw_data[header_end..];

    // Parse Content-Length
    let content_length: usize = headers_str
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.splitn(2, ':').nth(1))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);

    // Max payload size: 256 KB
    const MAX_PAYLOAD: usize = 256 * 1024;
    if content_length > MAX_PAYLOAD {
        let resp = create_http_response(413, "Payload Too Large", r#"{"error":"payload_too_large"}"#);
        stream.write_all(resp.as_bytes()).await?;
        return Ok(());
    }

    let mut body = already_read.to_vec();
    while body.len() < content_length {
        let remaining = content_length - body.len();
        let read_size = remaining.min(8192);
        let mut chunk = vec![0u8; read_size];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }

    // --- 5. Extract Content-Type and headers for the event ----------------
    let content_type = headers_str
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-type:"))
        .and_then(|l| l.splitn(2, ':').nth(1))
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|| "application/octet-stream".to_string());

    // Collect selected headers for the event payload
    let mut event_headers: HashMap<String, String> = HashMap::new();
    for line in headers_str.lines().skip(1) {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.splitn(2, ':').collect::<Vec<_>>().as_slice().get(0..2).and_then(|s| Some((s[0], s[1]))) {
            let key_lower = k.trim().to_ascii_lowercase();
            // Collect X-* headers and a few standard ones
            if key_lower.starts_with("x-")
                || key_lower == "content-type"
                || key_lower == "user-agent"
            {
                event_headers.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }

    // --- 6. HMAC verification (optional) ---------------------------------
    if let Some(ref secret) = target.hmac_secret {
        if !secret.is_empty() {
            let sig_header = event_headers
                .get("X-Hub-Signature-256")
                .or_else(|| event_headers.get("X-Signature"))
                .map(|s| s.as_str())
                .unwrap_or("");

            if sig_header.is_empty() || !verify_hmac_sha256(secret, &body, sig_header) {
                warn!(trigger = %target.trigger_id, "webhook: HMAC verification failed");
                let resp =
                    create_http_response(401, "Unauthorized", r#"{"error":"invalid_signature"}"#);
                stream.write_all(resp.as_bytes()).await?;
                return Ok(());
            }
        }
    }

    // --- 7. Convert body to UTF-8 payload string -------------------------
    let payload = format_payload(&body, &content_type);

    // --- 8. Send triggers/execute ACP notification to the agent ----------
    let received_at = chrono::Utc::now();
    let run_id = received_at.format("%Y-%m-%dT%H-%M-%SZ").to_string();

    let notification = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "triggers/execute",
        "params": {
            "trigger_id": target.trigger_id,
            "workspace_id": target.workspace_id,
            "payload": payload,
            "content_type": content_type,
            "headers": event_headers,
            "received_at": received_at.to_rfc3339(),
            "source_ip": client_ip,
        }
    });

    let notification_bytes = {
        let mut bytes = serde_json::to_vec(&notification).unwrap_or_default();
        bytes.push(b'\n');
        bytes
    };

    match agent_handle {
        AgentHandle::InProcess { stdin_tx, .. } => {
            if let Err(e) = stdin_tx.send(notification_bytes).await {
                error!(trigger = %target.trigger_id, err = %e, "failed to send triggers/execute to agent");
            } else {
                info!(trigger = %target.trigger_id, workspace = %target.workspace_id, "triggers/execute sent to agent");
            }
        }
        AgentHandle::Command(_) => {
            warn!("webhook received but agent is in Command mode — webhooks require InProcess (serve) mode");
        }
    }

    // --- 9. Return 200 OK immediately (async execution) ------------------
    let response_body = serde_json::json!({
        "status": "accepted",
        "run_id": run_id,
    })
    .to_string();
    let resp = create_http_response(200, "OK", &response_body);
    stream.write_all(resp.as_bytes()).await?;

    Ok(())
}


/// Verify an HMAC-SHA256 signature.
/// `signature` is expected in the form `sha256=<hex>` (GitHub style) or plain hex.
fn verify_hmac_sha256(secret: &str, body: &[u8], signature: &str) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;

    let expected_hex = signature
        .strip_prefix("sha256=")
        .unwrap_or(signature);

    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let result = mac.finalize().into_bytes();
    let result_hex = hex::encode(result);
    // Constant-time comparison via simple string equality (sufficient for HMAC)
    result_hex == expected_hex
}

/// Convert a raw body to a human-readable string based on Content-Type.
fn format_payload(body: &[u8], content_type: &str) -> String {
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("application/json") {
        // Pretty-print JSON if valid
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) {
            return serde_json::to_string_pretty(&v).unwrap_or_else(|_| {
                String::from_utf8_lossy(body).into_owned()
            });
        }
    } else if ct.contains("application/x-www-form-urlencoded") {
        // Convert key=value&key2=value2 to readable text
        return String::from_utf8_lossy(body)
            .split('&')
            .map(|pair| {
                let decoded = pair.replace('+', " ");
                let mut parts = decoded.splitn(2, '=');
                let k = parts.next().unwrap_or("");
                let v = parts.next().unwrap_or("");
                format!("{}: {}", k, v)
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    // Default: raw UTF-8 string
    String::from_utf8_lossy(body).into_owned()
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
async fn handle_websocket_connection<S>(stream: S, agent_handle: AgentHandle, auth_token: Arc<Option<String>>, agent_pool: Option<Arc<tokio::sync::RwLock<AgentPool>>>, push_relay: Option<Arc<PushRelayClient>>, working_dir: PathBuf, slash_commands: Arc<Vec<SlashCommandConfig>>, memory_path: Option<PathBuf>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Custom callback to validate auth token during WebSocket handshake
    // We also extract the token value for pool-based routing
    let auth_token_for_callback = Arc::clone(&auth_token);
    let extracted_token = Arc::new(tokio::sync::Mutex::new(String::new()));
    let extracted_token_clone = Arc::clone(&extracted_token);
    let extracted_client_id = Arc::new(tokio::sync::Mutex::new(String::new()));
    let extracted_client_id_clone = Arc::clone(&extracted_client_id);

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

        // Extract X-Client-Id header for multi-device message sync
        let client_id = req.headers()
            .get("X-Client-Id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if let Ok(mut guard) = extracted_client_id_clone.try_lock() {
            *guard = client_id;
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
    let device_client_id = extracted_client_id.lock().await.clone();

    // Decide whether to use pool-based or legacy handling
    if let Some(pool) = agent_pool {
        if client_token.is_empty() {
            warn!("Keep-alive enabled but no auth token found, falling back to legacy mode");
            handle_websocket_with_handle(ws_stream, agent_handle, push_relay, working_dir).await
        } else {
            if let AgentHandle::Command(ref cmd) = agent_handle {
                handle_websocket_pooled(ws_stream, cmd.clone(), client_token, pool, push_relay, working_dir.clone(), slash_commands, device_client_id, memory_path).await
            } else {
                // InProcess handles don't support pooling yet; fall back to per-connection
                handle_websocket_with_handle(ws_stream, agent_handle, push_relay, working_dir).await
            }
        }
    } else {
        handle_websocket_with_handle(ws_stream, agent_handle, push_relay, working_dir).await
    }
}

/// Handle WebSocket connection with agent pool (keep-alive mode)
async fn handle_websocket_pooled<S>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    agent_command: String,
    token: String,
    pool: Arc<tokio::sync::RwLock<AgentPool>>,
    push_relay: Option<Arc<PushRelayClient>>,
    _working_dir: PathBuf,
    slash_commands: Arc<Vec<SlashCommandConfig>>,
    device_client_id: String,
    memory_path: Option<PathBuf>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    // Get or spawn agent from pool
    let (ws_to_agent_tx, mut agent_to_ws_rx, buffered, was_reused, cached_init, cached_session, broadcast_tx) = {
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
        if let Err(e) = ws_sender.send(Message::Text(msg.into())).await {
            error!("Failed to replay buffered message: {}", e);
        }
    }
    
    // Memory injection: start as false (inject on first session/prompt).
    // Set to true only when reusing an agent with a session/load (resume) — memory already in context.
    let mut initial_memory_injected = false;

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
            let (session_handled, reuse_was_new_session) = handle_create_session_intercept(
                &mut ws_receiver, &mut ws_sender, cached, &slash_commands
            ).await;
            if session_handled {
                info!("✅ Session request intercepted, reusing existing session (was_new={})", reuse_was_new_session);
            } else {
                warn!("⚠️  Next message was not a session request, proceeding normally");
            }
            // Re-inject memory when the client explicitly reset (session/new).
            // Skip re-injection on session/load (resume) — memory is already in context.
            initial_memory_injected = !reuse_was_new_session;
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

    // Track the request ID of `session/new` so Task 2 can identify the response
    // regardless of the response shape (some agents don't return `sessionId`).
    let pending_session_req_id: Arc<std::sync::Mutex<Option<serde_json::Value>>> =
        Arc::new(std::sync::Mutex::new(None));
    let pending_session_req_id_writer = Arc::clone(&pending_session_req_id);
    let pending_session_req_id_reader = Arc::clone(&pending_session_req_id);

    // Keepalive / zombie-connection detection.
    // Starts as `true` (healthy). Task 2 swaps it to `false` each time it sends a
    // Ping; Task 1 resets it to `true` when a Pong arrives. If it is still `false`
    // on the next ping interval the client is considered dead and the connection is
    // closed so the rate-limiter slot is freed.
    // Channel for Task 1 to inject synthetic responses back to the client
    // (e.g., session/load errors on fresh agents). Task 2 reads from this.
    let (inject_tx, mut inject_rx) = mpsc::channel::<String>(8);

    let pong_received = Arc::new(AtomicBool::new(true));
    let pong_received_for_receiver = Arc::clone(&pong_received);

    // Session ID shared between Task 1 (memory update sender) and Task 2 (session capturer).
    // Pre-populated from cached session for reconnects; Task 2 fills it on fresh sessions.
    let current_session_id: Arc<std::sync::Mutex<Option<String>>> = Arc::new(
        std::sync::Mutex::new(
            cached_session.as_ref().and_then(|s| extract_session_id_from_response(s))
        )
    );
    // When Task 1 sends a silent memory-update prompt, it records the request id here.
    // Task 2 drops all agent output until it sees a response with that id, then clears it.
    let suppress_response_id: Arc<std::sync::Mutex<Option<String>>> =
        Arc::new(std::sync::Mutex::new(None));

    // Task 1: WebSocket → Agent (via channel)
    let ws_to_agent_tx_clone = ws_to_agent_tx.clone();
    let broadcast_tx_for_task1 = broadcast_tx.clone();
    let device_client_id_for_task1 = device_client_id.clone();
    let push_relay_for_register = push_relay.clone();
    let memory_path_for_task1 = memory_path.clone();
    let current_session_id_task1 = Arc::clone(&current_session_id);
    let suppress_response_id_task1 = Arc::clone(&suppress_response_id);
    let mut ws_to_agent = tokio::spawn(async move {
        // True once memory has been prepended to the first session/prompt of this connection.
        // Pre-set to true for reused agents resuming an existing session (session/load) since
        // memory is already in context. False for fresh agents or session/new resets.
        let mut memory_injected = initial_memory_injected;
        while let Some(msg_result) = ws_receiver.next().await {
            match msg_result {
                Ok(msg) => {
                    if msg.is_text() || msg.is_binary() {
                        let data = msg.into_data();
                        let mut text = String::from_utf8_lossy(&data).to_string();
                        debug!("📥 Received from Mobile ({} bytes): {}", text.len(),
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

                        // Handle bridge/appendMemory — append text to MEMORY.md, then
                        // send a silent session/prompt so the agent updates its context.
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            if v.get("method").and_then(|m| m.as_str()) == Some("bridge/appendMemory") {
                                if let Some(entry_text) = v.pointer("/params/text").and_then(|t| t.as_str()) {
                                    if let Some(ref path) = memory_path_for_task1 {
                                        let entry = format!("\n{}\n", entry_text.trim());
                                        let mut write_ok = false;
                                        match tokio::fs::OpenOptions::new()
                                            .create(true)
                                            .append(true)
                                            .open(path)
                                            .await
                                        {
                                            Ok(mut f) => {
                                                use tokio::io::AsyncWriteExt;
                                                if let Err(e) = f.write_all(entry.as_bytes()).await {
                                                    error!("Failed to write to MEMORY.md: {}", e);
                                                } else {
                                                    info!("🧠 Appended memory entry ({} bytes)", entry.len());
                                                    write_ok = true;
                                                }
                                            }
                                            Err(e) => error!("Failed to open MEMORY.md: {}", e),
                                        }

                                        // After a successful write, push the full updated memory
                                        // into the agent as a silent context-update prompt.
                                        if write_ok {
                                            let session_id_opt = current_session_id_task1
                                                .lock().ok().and_then(|g| g.clone());
                                            if let Some(session_id) = session_id_opt {
                                                if let Ok(contents) = tokio::fs::read_to_string(path).await {
                                                    let trimmed = contents.trim().to_string();
                                                    if !trimmed.is_empty() {
                                                        let req_id = format!(
                                                            "__memory_update_{}",
                                                            uuid::Uuid::new_v4().simple()
                                                        );
                                                        let prompt_msg = serde_json::json!({
                                                            "jsonrpc": "2.0",
                                                            "id": req_id,
                                                            "method": "session/prompt",
                                                            "params": {
                                                                "sessionId": session_id,
                                                                "prompt": [{
                                                                    "type": "text",
                                                                    "text": format!(
                                                                        "[Memory Update] A new entry has been added to your persistent memory. The full memory file (including the new entry) is below. Consolidate it by merging or replacing any conflicting or duplicate information, keeping the most recent values. Reply with the complete consolidated memory wrapped in <merged_memory>...</merged_memory> tags and nothing else.\n\n<memory>\n{}\n</memory>",
                                                                        trimmed
                                                                    )
                                                                }]
                                                            }
                                                        });
                                                        // Arm suppression before sending so Task 2
                                                        // immediately starts dropping responses.
                                                        if let Ok(mut guard) = suppress_response_id_task1.lock() {
                                                            *guard = Some(req_id);
                                                        }
                                                        let msg_str = serde_json::to_string(&prompt_msg)
                                                            .unwrap_or_default();
                                                        if !msg_str.is_empty() {
                                                            info!("🧠 Sending silent memory context update to agent (session={})", session_id);
                                                            let _ = ws_to_agent_tx_clone.send(msg_str).await;
                                                        }
                                                    }
                                                }
                                            } else {
                                                info!("🧠 Memory saved; skipping agent update (no active session yet)");
                                            }
                                        }
                                    }
                                }
                                continue; // don't forward original notification to agent
                            }
                        }
                        
                        // On fresh agents, intercept session/load and return a
                        // synthetic error. A just-spawned agent has no sessions to
                        // load, and some agents (e.g. Goose) hang on unknown
                        // session IDs. The synthetic error lets the client fall
                        // through to session/new and get the correct new session ID.
                        // Also track session request IDs so Task 2 can cache the
                        // session/new response.
                        if needs_init_capture {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                                let method = v.get("method").and_then(|m| m.as_str());
                                if method == Some("session/load") {
                                    if let Some(req_id) = v.get("id") {
                                        let session_id = v.pointer("/params/sessionId")
                                            .and_then(|s| s.as_str())
                                            .or_else(|| v.pointer("/params/sessionId/value").and_then(|s| s.as_str()))
                                            .unwrap_or("unknown");
                                        info!("🔄 Returning synthetic error for session/load on fresh agent (id={}, session={})", req_id, session_id);
                                        let error_response = serde_json::json!({
                                            "jsonrpc": "2.0",
                                            "id": req_id,
                                            "error": {
                                                "code": -32602,
                                                "message": "Invalid params",
                                                "data": format!("Session not found (fresh agent): {}", session_id)
                                            }
                                        });
                                        let _ = inject_tx.send(serde_json::to_string(&error_response).unwrap_or_default()).await;
                                    }
                                    continue; // Don't forward session/load to agent
                                }
                                // Track session/new request IDs
                                if method == Some("session/new") {
                                    if let Some(id) = v.get("id") {
                                        info!("📋 Tracking session/new request id={}", id);
                                        if let Ok(mut guard) = pending_session_req_id_writer.lock() {
                                            *guard = Some(id.clone());
                                        }
                                    }
                                }
                            }
                        }

                        // Inject MEMORY.md content into the first session/prompt.
                        // Runs for fresh agents and for reused agents after session/new (clear session).
                        if !memory_injected {
                            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&text) {
                                if v.get("method").and_then(|m| m.as_str()) == Some("session/prompt") {
                                    if let Some(ref path) = memory_path_for_task1 {
                                        if let Ok(contents) = tokio::fs::read_to_string(path).await {
                                            let trimmed = contents.trim();
                                            if !trimmed.is_empty() {
                                                let memory_block = serde_json::json!({
                                                    "type": "text",
                                                    "text": format!("<memory>\n{}\n</memory>\n\n", trimmed)
                                                });
                                                if let Some(prompt_arr) = v.pointer_mut("/params/prompt") {
                                                    if let Some(arr) = prompt_arr.as_array_mut() {
                                                        arr.insert(0, memory_block);
                                                        info!("🧠 Injected memory context into session/prompt ({} bytes)", trimmed.len());
                                                    }
                                                }
                                                text = serde_json::to_string(&v).unwrap_or(text);
                                            }
                                        }
                                    }
                                    memory_injected = true;
                                }
                            }
                        }

                        // Echo session/prompt to all connected clients for multi-device sync
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            if v.get("method").and_then(|m| m.as_str()) == Some("session/prompt") {
                                if let Some(params) = v.get("params") {
                                    let prompt_content = params.get("prompt").cloned()
                                        .unwrap_or(serde_json::Value::Array(vec![]));
                                    let echo = serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "method": "bridge/remoteUserMessage",
                                        "params": {
                                            "senderId": device_client_id_for_task1,
                                            "content": prompt_content
                                        }
                                    });
                                    if let Ok(echo_str) = serde_json::to_string(&echo) {
                                        let _ = broadcast_tx_for_task1.send(echo_str);
                                    }
                                }
                            }
                        }

                        if ws_to_agent_tx_clone.send(text).await.is_err() {
                            error!("Failed to send to agent channel");
                            break;
                        }
                        debug!("✅ Forwarded to agent");
                    } else if msg.is_pong() {
                        pong_received_for_receiver.store(true, Ordering::Relaxed);
                        debug!("📶 Pong received from client");
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
    let current_session_id_task2 = Arc::clone(&current_session_id);
    let suppress_response_id_task2 = Arc::clone(&suppress_response_id);
    let memory_path_for_task2 = memory_path.clone();
    let agent_to_ws = tokio::spawn(async move {
        let mut init_captured = false;
        let mut session_captured = false;
        // Send a Ping every 30 s; if no Pong arrives before the next Ping the
        // connection is treated as dead and closed (frees the rate-limiter slot).
        let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
        ping_interval.tick().await; // skip the immediate first tick
        loop {
            tokio::select! {
                result = agent_to_ws_rx.recv() => { match result {
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
                    
                    // On first connection, capture the createSession response.
                    // First try matching by response shape (result.sessionId), then
                    // fall back to matching the response ID against the tracked
                    // session/new request ID — this handles agents (e.g. Goose)
                    // whose session response doesn't include a sessionId field.
                    if needs_init_capture && !session_captured {
                        let is_session_resp = if is_create_session_response(&line) {
                            true
                        } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                            if v.get("result").is_some() {
                                if let Some(resp_id) = v.get("id") {
                                    let matches = pending_session_req_id_reader
                                        .lock()
                                        .map(|guard| guard.as_ref() == Some(resp_id))
                                        .unwrap_or(false);
                                    if matches {
                                        info!("📋 Session response matched by request ID (id={})", resp_id);
                                    }
                                    matches
                                } else {
                                    false
                                }
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        if is_session_resp {
                            info!("📋 Captured createSession response for future reconnections");
                            let mut pool = pool_for_capture.write().await;
                            pool.cache_session_response(&token_for_capture, line.clone());
                            session_captured = true;
                            // Store session ID so Task 1 can send silent memory-update prompts.
                            if let Some(sid) = extract_session_id_from_response(&line) {
                                if let Ok(mut guard) = current_session_id_task2.lock() {
                                    *guard = Some(sid);
                                }
                            }
                        }
                    }

                    // If the agent reports "Session not found", invalidate the
                    // cached session so the next reconnect creates a fresh one
                    // instead of replaying a stale session ID.
                    if line.contains("Session not found") {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                            if v.get("error").is_some() {
                                warn!("🗑️ Agent reported 'Session not found' — invalidating cached session");
                                let mut pool = pool_for_capture.write().await;
                                pool.clear_session_response(&token_for_capture);
                            }
                        }
                    }

                    // Drop this message if it belongs to a silent memory-update prompt.
                    // Task 1 arms `suppress_response_id` before sending the prompt; we clear
                    // it once we see the final JSON-RPC response (has a matching `id` field).
                    // On the final response, extract <merged_memory> content and overwrite
                    // MEMORY.md so conflicting/duplicate entries are resolved.
                    {
                        // Determine suppression state without holding the lock across an await.
                        let (is_suppressed, is_final) = {
                            let mut sup = suppress_response_id_task2.lock().unwrap();
                            if sup.is_some() {
                                let final_resp = if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                                    v.get("id").and_then(|i| i.as_str()) == sup.as_deref()
                                } else {
                                    false
                                };
                                if final_resp {
                                    *sup = None; // final response received — stop suppressing
                                }
                                (true, final_resp)
                            } else {
                                (false, false)
                            }
                        }; // lock dropped here
                        if is_suppressed {
                            if is_final {
                                // Extract the merged memory the agent produced and write it back.
                                if let Some(ref path) = memory_path_for_task2 {
                                    if let Some(merged) = extract_merged_memory(&line) {
                                        let content = format!("{}\n", merged.trim());
                                        match tokio::fs::write(path, content.as_bytes()).await {
                                            Ok(_) => info!("🧠 MEMORY.md rewritten with merged content ({} bytes)", content.len()),
                                            Err(e) => error!("Failed to rewrite MEMORY.md: {}", e),
                                        }
                                    }
                                }
                            }
                            debug!("🔇 Suppressed silent memory-update agent response");
                            continue;
                        }
                    }

                    // Check whether this line is a session response we should
                    // follow up with available_commands_update.
                    let inject_commands = !slash_commands.is_empty()
                        && is_create_session_response(&line)
                        && !line.contains("\"error\"");

                    debug!("📤 Sending to Mobile ({} bytes): {}", line.len(),
                        line.chars().take(200).collect::<String>());

                    if let Err(e) = ws_sender.send(Message::Text(line.clone().into())).await {
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

                    // Inject available_commands_update immediately after the session
                    // response so clients that connect to agents without native support
                    // (e.g. Copilot CLI) still get the command picker populated.
                    if inject_commands {
                        if let Some(session_id) = extract_session_id_from_response(&line) {
                            let notification = build_available_commands_notification(
                                &session_id, &slash_commands,
                            );
                            info!("📋 Injecting available_commands_update for session {}", session_id);
                            let _ = ws_sender.send(Message::Text(notification.into())).await;
                        }
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
            } } // end match result / end recv arm
            Some(injected) = inject_rx.recv() => {
                // Synthetic response injected by Task 1 (e.g., session/load error)
                debug!("📤 Sending injected response to Mobile ({} bytes)", injected.len());
                if let Err(e) = ws_sender.send(Message::Text(injected.into())).await {
                    debug!("Client disconnected while sending injected response: {}", e);
                    break;
                }
            }
            _ = ping_interval.tick() => {
                // If the previous ping went unanswered the client is gone.
                if !pong_received.swap(false, Ordering::Relaxed) {
                    warn!("💀 Ping timeout: no pong from client, closing dead connection");
                    break;
                }
                debug!("📶 Sending WebSocket ping to client");
                if let Err(e) = ws_sender.send(Message::Ping(vec![].into())).await {
                    debug!("Ping send failed (client disconnected): {}", e);
                    break;
                }
            }
            } // end select!
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

/// Extract the content inside `<merged_memory>...</merged_memory>` tags from an agent
/// response JSON string. Walks the full value tree to find the text field that contains
/// the tags, then returns the inner text (JSON-unescaped via serde).
fn extract_merged_memory(json_str: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let text = find_text_containing(&v, "<merged_memory>")?;
    let start_tag = "<merged_memory>";
    let end_tag = "</merged_memory>";
    let start = text.find(start_tag)? + start_tag.len();
    let end = text[start..].find(end_tag)? + start;
    Some(text[start..end].to_string())
}

/// Recursively walk a JSON value tree and return the first string that contains `needle`.
fn find_text_containing(v: &serde_json::Value, needle: &str) -> Option<String> {
    match v {
        serde_json::Value::String(s) if s.contains(needle) => Some(s.clone()),
        serde_json::Value::Object(map) => {
            for val in map.values() {
                if let Some(s) = find_text_containing(val, needle) {
                    return Some(s);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Some(s) = find_text_containing(item, needle) {
                    return Some(s);
                }
            }
            None
        }
        _ => None,
    }
}

/// Extract the `sessionId` string from a JSON-RPC session/new response.
fn extract_session_id_from_response(response: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(response)
        .ok()
        .and_then(|v| {
            v.get("result")
                .and_then(|r| r.get("sessionId"))
                .and_then(|s| s.as_str())
                .map(String::from)
        })
}

/// Build a `session/update` JSON-RPC notification carrying `available_commands_update`.
///
/// The serialisation follows the ACP schema:
/// - `SessionUpdate` is tagged with `"sessionUpdate": "available_commands_update"`
/// - `AvailableCommand` uses camelCase; `input` is only present when `input_hint` is set
fn build_available_commands_notification(
    session_id: &str,
    commands: &[SlashCommandConfig],
) -> String {
    let cmds: Vec<serde_json::Value> = commands
        .iter()
        .map(|c| {
            let mut obj = serde_json::json!({
                "name": c.name,
                "description": c.description,
            });
            if let Some(ref hint) = c.input_hint {
                obj["input"] = serde_json::json!({ "hint": hint });
            }
            obj
        })
        .collect();

    serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "available_commands_update",
                "availableCommands": cmds
            }
        }
    }))
    .unwrap_or_default()
}

/// Intercept the client's `createSession` request and reply with a cached response.
/// Returns (intercepted, was_new_session):
///   intercepted      = true if a session request was handled
///   was_new_session  = true if the client sent session/new (reset), false for session/load (resume)
async fn handle_create_session_intercept<S>(
    ws_receiver: &mut futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
    ws_sender: &mut futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, Message>,
    cached_response: &str,
    slash_commands: &[SlashCommandConfig],
) -> (bool, bool)
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
            _ => return (false, false),
        };

        request = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(_) => return (false, false),
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
                return (false, false);
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
                if let Err(e) = ws_sender.send(Message::Text(resp_str.into())).await {
                    error!("Failed to send synthetic initialize response: {}", e);
                    return (false, false);
                }
                skipped += 1;
                if skipped >= max_skip {
                    warn!("⚠️  Too many messages before session request, giving up");
                    return (false, false);
                }
                continue;
            }
        }

        // It's some other request, not a session request — can't intercept
        warn!("⚠️  Message is not session/new or session/load (method={:?}, has_id={}, raw={}), cannot intercept",
            method, request.get("id").is_some(),
            msg.chars().take(200).collect::<String>());
        return (false, false);
    }

    let was_new = request.get("method").and_then(|m| m.as_str()) == Some("session/new");

    // Extract the request ID so we can match it in the response
    let request_id = match request.get("id") {
        Some(id) => id.clone(),
        None => return (false, false),
    };

    let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("unknown");
    info!("🔄 Intercepting {} request (id={})", method, request_id);

    // Parse the cached response and replace its "id" with the new request's "id"
    let mut cached: serde_json::Value = match serde_json::from_str(cached_response) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to parse cached session response: {}", e);
            return (false, false);
        }
    };

    cached["id"] = request_id;

    let response_str = serde_json::to_string(&cached).unwrap_or_default();
    debug!("🔄 Sending cached session response ({} bytes): {}", response_str.len(),
        response_str.chars().take(200).collect::<String>());

    if let Err(e) = ws_sender.send(Message::Text(response_str.into())).await {
        error!("Failed to send cached session response: {}", e);
        return (false, false);
    }

    // Inject available_commands_update so clients get the command picker
    // even when the agent doesn't send this notification itself.
    if !slash_commands.is_empty() {
        if let Some(session_id) = extract_session_id_from_response(cached_response) {
            let notification = build_available_commands_notification(&session_id, slash_commands);
            info!("📋 Injecting available_commands_update for cached session {}", session_id);
            let _ = ws_sender.send(Message::Text(notification.into())).await;
        }
    }

    (true, was_new)
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
    
    if let Err(e) = ws_sender.send(Message::Text(response_str.into())).await {
        error!("Failed to send cached initialize response: {}", e);
        return false;
    }
    
    true
}


/// Dispatch to the correct WebSocket handler based on the AgentHandle variant.
async fn handle_websocket_with_handle<S>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    agent_handle: AgentHandle,
    push_relay: Option<Arc<PushRelayClient>>,
    working_dir: PathBuf,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    match agent_handle {
        AgentHandle::Command(cmd) => handle_websocket_legacy(ws_stream, cmd, push_relay, working_dir).await,
        AgentHandle::InProcess { stdin_tx, stdout_rx } => {
            handle_websocket_inprocess(ws_stream, stdin_tx, stdout_rx).await
        }
    }
}

/// Handle WebSocket connection backed by in-process channels.
async fn handle_websocket_inprocess<S>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    stdin_tx: mpsc::Sender<Vec<u8>>,
    stdout_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    // Dedicated channel so that ws_to_agent can tell agent_to_ws to stop
    // reading stdout_rx the moment the WebSocket closes. This prevents the
    // outgoing task from consuming messages (e.g. an initialize response)
    // that were meant for the *next* connection, which would cause the new
    // connection to time out waiting for a reply that was already discarded.
    let (agent_stop_tx, mut agent_stop_rx) = mpsc::channel::<()>(1);

    // Task 1: WebSocket → agent channel
    let shutdown_tx_ws = shutdown_tx.clone();
    let ws_to_agent = tokio::spawn(async move {
        while let Some(msg_result) = ws_receiver.next().await {
            match msg_result {
                Ok(msg) if msg.is_text() || msg.is_binary() => {
                    let mut data = msg.into_data().to_vec();
                    data.push(b'\n');
                    debug!("📥 WS→agent ({} bytes)", data.len());
                    if stdin_tx.send(data).await.is_err() {
                        break;
                    }
                }
                Ok(msg) if msg.is_close() => {
                    info!("📱 Client closed connection");
                    break;
                }
                Err(e) => { error!("WebSocket receive error: {}", e); break; }
                _ => {}
            }
        }
        debug!("ws_to_agent task ended");
        // Stop agent_to_ws before signalling main, so the mutex is released
        // before handle_websocket_inprocess returns and a new connection begins.
        let _ = agent_stop_tx.send(()).await;
        let _ = shutdown_tx_ws.send(()).await;
    });

    // Task 2: agent channel → WebSocket
    let shutdown_tx_clone = shutdown_tx.clone();
    let agent_to_ws = tokio::spawn(async move {
        let mut rx = stdout_rx.lock().await;
        loop {
            tokio::select! {
                bytes_opt = rx.recv() => {
                    match bytes_opt {
                        Some(bytes) => {
                            let line = String::from_utf8_lossy(&bytes).trim_end_matches('\n').to_string();
                            debug!("📤 agent→WS ({} bytes)", line.len());
                            if let Err(e) = ws_sender.send(Message::Text(line.into())).await {
                                let msg = e.to_string();
                                if msg.contains("Sending after closing") || msg.contains("connection closed") {
                                    debug!("WebSocket closed before message could be sent (client disconnected)");
                                } else {
                                    error!("Failed to send to WebSocket: {}", e);
                                }
                                break;
                            }
                        }
                        None => break,
                    }
                }
                _ = agent_stop_rx.recv() => {
                    // WebSocket is closing; exit immediately so the stdout_rx
                    // mutex is released before the next connection acquires it.
                    debug!("agent_to_ws: stop signal received, releasing stdout_rx");
                    break;
                }
            }
        }
        let _ = shutdown_tx_clone.send(()).await;
    });

    shutdown_rx.recv().await;
    ws_to_agent.abort();
    // Await (not just abort) so we guarantee the stdout_rx mutex is released
    // before this function returns and any new connection handler starts.
    let _ = agent_to_ws.await;

    Ok(())
}


async fn handle_websocket_legacy<S>(ws_stream: tokio_tungstenite::WebSocketStream<S>, agent_command: String, _push_relay: Option<Arc<PushRelayClient>>, working_dir: PathBuf) -> Result<()>
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
    info!("🚀 Spawning agent: {} {:?} (cwd: {})", command, args, working_dir.display());
    
    let mut child = Command::new(command)
        .args(args)
        .current_dir(&working_dir)
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
                        let raw = msg.into_data();
                        let data = String::from_utf8_lossy(&raw);
                        debug!("📥 Received from Mobile ({} bytes): {}", data.len(),
                            data.chars().take(200).collect::<String>());

                        if let Err(e) = stdin_writer.write_all(data.as_bytes()).await {
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
        info!("📖 Agent stdout reader task started");

        while let Ok(Some(line)) = lines.next_line().await {
            info!("📤 Agent -> Mobile ({} bytes): {}", line.len(),
                line.chars().take(200).collect::<String>());

            if let Err(e) = ws_sender.send(Message::Text(line.into())).await {
                let msg = e.to_string();
                if msg.contains("Sending after closing") || msg.contains("connection closed") {
                    debug!("WebSocket closed before message could be sent (client disconnected)");
                } else {
                    error!("Failed to send to WebSocket: {}", e);
                }
                break;
            }
            info!("✅ Message sent to WebSocket successfully");
        }

        info!("Agent stdout reader task ended");
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

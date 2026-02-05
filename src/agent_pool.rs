use anyhow::{Context, Result};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{debug, error, info, warn};

/// Configuration for the agent pool
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// How long to keep idle agents alive (no client connected)
    pub idle_timeout: Duration,
    /// Maximum number of concurrent agent processes
    pub max_agents: usize,
    /// Whether to buffer agent messages while client is disconnected
    pub buffer_messages: bool,
    /// Maximum number of buffered messages per agent
    pub max_buffer_size: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(1800),
            max_agents: 10,
            buffer_messages: false,
            max_buffer_size: 1000,
        }
    }
}

/// A pooled agent process with its I/O handles
pub struct PooledAgent {
    /// The spawned child process
    process: Child,
    /// Sender for messages going to the agent (from WebSocket to stdin)
    pub ws_to_agent_tx: mpsc::Sender<String>,
    /// Broadcast sender for messages from agent stdout.
    /// Each new connection subscribes via .subscribe()
    pub agent_to_ws_tx: broadcast::Sender<String>,
    /// Whether a client is currently connected
    pub connected: bool,
    /// When the client last disconnected (for idle timeout)
    pub disconnected_at: Option<Instant>,
    /// Buffered messages from agent while client was disconnected
    pub message_buffer: Vec<String>,
    /// Cached `initialize` response from the agent (raw JSON-RPC result).
    /// On reconnect we intercept the client's `initialize` request and reply
    /// with this cached response instead of forwarding to the agent.
    pub cached_init_response: Option<String>,
    /// The agent command used to spawn this agent
    #[allow(dead_code)]
    pub agent_command: String,
}

impl PooledAgent {
    /// Check if this agent's process is still running
    pub fn is_alive(&mut self) -> bool {
        match self.process.try_wait() {
            Ok(Some(_)) => false,
            Ok(None) => true,
            Err(_) => false,
        }
    }

    /// Kill the agent process gracefully
    pub async fn kill(&mut self) {
        info!("Killing pooled agent process");
        if let Err(e) = self.process.kill().await {
            warn!("Failed to kill agent process: {}", e);
        }
    }

    /// Subscribe to agent stdout messages
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.agent_to_ws_tx.subscribe()
    }
}

/// Manages a pool of long-lived agent processes keyed by auth token
pub struct AgentPool {
    pub(crate) agents: HashMap<String, PooledAgent>,
    config: PoolConfig,
}

impl AgentPool {
    pub fn new(config: PoolConfig) -> Self {
        Self {
            agents: HashMap::new(),
            config,
        }
    }

    /// Get an existing agent or spawn a new one for the given token.
    /// Returns (ws_to_agent_tx, agent_to_ws_rx, buffered_messages, was_reused, cached_init_response)
    pub async fn get_or_spawn(
        &mut self,
        token: &str,
        agent_command: &str,
    ) -> Result<(mpsc::Sender<String>, broadcast::Receiver<String>, Vec<String>, bool, Option<String>)> {
        // Check if we have an existing agent for this token
        if let Some(agent) = self.agents.get_mut(token) {
            if agent.is_alive() {
                info!("Reusing existing agent for token (keep-alive)");
                agent.connected = true;
                agent.disconnected_at = None;

                let buffered = std::mem::take(&mut agent.message_buffer);
                if !buffered.is_empty() {
                    info!("Replaying {} buffered messages", buffered.len());
                }

                let tx = agent.ws_to_agent_tx.clone();
                let rx = agent.subscribe();
                let cached_init = agent.cached_init_response.clone();

                return Ok((tx, rx, buffered, true, cached_init));
            } else {
                info!("Agent process died, removing from pool");
                self.agents.remove(token);
            }
        }

        // Check max agents limit
        if self.agents.len() >= self.config.max_agents {
            let oldest_idle = self
                .agents
                .iter()
                .filter(|(_, a)| !a.connected)
                .min_by_key(|(_, a)| a.disconnected_at)
                .map(|(k, _)| k.clone());

            if let Some(key) = oldest_idle {
                info!("Evicting oldest idle agent to make room");
                if let Some(mut agent) = self.agents.remove(&key) {
                    agent.kill().await;
                }
            } else {
                anyhow::bail!(
                    "Agent pool is full ({} agents, all connected). Cannot spawn new agent.",
                    self.config.max_agents
                );
            }
        }

        // Spawn a new agent
        info!("Spawning new pooled agent");
        self.spawn_agent(token, agent_command).await
    }

    /// Spawn a new agent process and set up I/O channels
    async fn spawn_agent(
        &mut self,
        token: &str,
        agent_command: &str,
    ) -> Result<(mpsc::Sender<String>, broadcast::Receiver<String>, Vec<String>, bool, Option<String>)> {
        let parts: Vec<&str> = agent_command.split_whitespace().collect();
        if parts.is_empty() {
            anyhow::bail!("Empty agent command");
        }

        let command = parts[0];
        let args = &parts[1..];

        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false)
            .spawn()
            .context(format!("Failed to spawn agent command: {}", agent_command))?;

        let stdin = child.stdin.take().context("Failed to open agent stdin")?;
        let stdout = child.stdout.take().context("Failed to open agent stdout")?;
        let stderr = child.stderr.take().context("Failed to open agent stderr")?;

        // Channel: WebSocket messages to agent stdin (mpsc)
        let (ws_to_agent_tx, mut ws_to_agent_rx) = mpsc::channel::<String>(100);

        // Channel: agent stdout to WebSocket (broadcast, supports reconnection)
        let (agent_to_ws_tx, agent_to_ws_rx) = broadcast::channel::<String>(256);

        // Background task: forward ws_to_agent_rx to agent stdin
        let mut stdin_writer = stdin;
        tokio::spawn(async move {
            while let Some(msg) = ws_to_agent_rx.recv().await {
                if let Err(e) = stdin_writer.write_all(msg.as_bytes()).await {
                    error!("Failed to write to pooled agent stdin: {}", e);
                    break;
                }
                if let Err(e) = stdin_writer.write_all(b"\n").await {
                    error!("Failed to write newline to pooled agent stdin: {}", e);
                    break;
                }
                if let Err(e) = stdin_writer.flush().await {
                    error!("Failed to flush pooled agent stdin: {}", e);
                    break;
                }
            }
            debug!("Pooled agent stdin writer task ended");
        });

        // Background task: forward agent stdout to broadcast channel
        let stdout_tx = agent_to_ws_tx.clone();
        let stdout_reader = BufReader::new(stdout);
        tokio::spawn(async move {
            let mut lines = stdout_reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                debug!(
                    "Pooled agent stdout ({} bytes): {}",
                    line.len(),
                    line.chars().take(200).collect::<String>()
                );
                // broadcast::send only fails if there are no receivers,
                // which is fine when no client is connected
                let _ = stdout_tx.send(line);
            }
            debug!("Pooled agent stdout reader task ended");
        });

        // Background task: log stderr
        let stderr_reader = BufReader::new(stderr);
        tokio::spawn(async move {
            let mut lines = stderr_reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                warn!("Pooled agent stderr: {}", line);
            }
            debug!("Pooled agent stderr reader task ended");
        });

        let pooled = PooledAgent {
            process: child,
            ws_to_agent_tx: ws_to_agent_tx.clone(),
            agent_to_ws_tx,
            connected: true,
            disconnected_at: None,
            message_buffer: Vec::new(),
            cached_init_response: None,
            agent_command: agent_command.to_string(),
        };

        self.agents.insert(token.to_string(), pooled);

        Ok((ws_to_agent_tx, agent_to_ws_rx, Vec::new(), false, None))
    }

    /// Mark a client as disconnected. The agent stays alive for idle_timeout.
    pub fn mark_disconnected(&mut self, token: &str) {
        if let Some(agent) = self.agents.get_mut(token) {
            info!("Client disconnected, agent entering idle state (keep-alive)");
            agent.connected = false;
            agent.disconnected_at = Some(Instant::now());
        }
    }

    /// Cache the agent's `initialize` response so reconnections can skip re-initialization
    pub fn cache_init_response(&mut self, token: &str, response: String) {
        if let Some(agent) = self.agents.get_mut(token) {
            info!("Cached initialize response for agent (keep-alive)");
            agent.cached_init_response = Some(response);
        }
    }

    /// Remove and kill an agent
    #[allow(dead_code)]
    pub async fn remove_agent(&mut self, token: &str) {
        if let Some(mut agent) = self.agents.remove(token) {
            agent.kill().await;
        }
    }

    /// Check for idle agents that have exceeded the timeout and kill them
    pub async fn reap_idle_agents(&mut self) {
        let timeout = self.config.idle_timeout;
        let mut to_remove = Vec::new();

        for (token, agent) in self.agents.iter_mut() {
            if !agent.is_alive() {
                info!("Agent for token {}... died, removing", &token[..8.min(token.len())]);
                to_remove.push(token.clone());
                continue;
            }

            if !agent.connected {
                if let Some(disconnected_at) = agent.disconnected_at {
                    if disconnected_at.elapsed() > timeout {
                        info!(
                            "Agent for token {}... idle for {:?}, terminating",
                            &token[..8.min(token.len())],
                            disconnected_at.elapsed()
                        );
                        to_remove.push(token.clone());
                    }
                }
            }
        }

        for token in to_remove {
            if let Some(mut agent) = self.agents.remove(&token) {
                agent.kill().await;
            }
        }
    }

    /// Get pool statistics
    pub fn stats(&self) -> PoolStats {
        let total = self.agents.len();
        let connected = self.agents.values().filter(|a| a.connected).count();
        let idle = total - connected;
        PoolStats {
            total,
            connected,
            idle,
            max: self.config.max_agents,
        }
    }

    /// Check if the pool contains an agent for the given token
    #[allow(dead_code)]
    pub fn contains(&self, token: &str) -> bool {
        self.agents.contains_key(token)
    }

    /// Kill a specific agent's process (for testing).
    /// Returns true if the agent existed.
    #[allow(dead_code)]
    pub async fn kill_agent(&mut self, token: &str) -> bool {
        if let Some(agent) = self.agents.get_mut(token) {
            agent.kill().await;
            true
        } else {
            false
        }
    }

    /// Buffer a message for a disconnected agent
    pub fn buffer_message(&mut self, token: &str, message: String) {
        if !self.config.buffer_messages {
            return;
        }
        if let Some(agent) = self.agents.get_mut(token) {
            if agent.message_buffer.len() < self.config.max_buffer_size {
                agent.message_buffer.push(message);
            } else {
                warn!("Message buffer full for agent, dropping message");
            }
        }
    }

    /// Shut down all agents in the pool
    #[allow(dead_code)]
    pub async fn shutdown_all(&mut self) {
        info!("Shutting down all pooled agents ({} total)", self.agents.len());
        let tokens: Vec<String> = self.agents.keys().cloned().collect();
        for token in tokens {
            if let Some(mut agent) = self.agents.remove(&token) {
                agent.kill().await;
            }
        }
    }
}

/// Pool statistics
#[derive(Debug)]
pub struct PoolStats {
    pub total: usize,
    pub connected: usize,
    pub idle: usize,
    pub max: usize,
}

impl std::fmt::Display for PoolStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AgentPool: {}/{} agents ({} connected, {} idle)",
            self.total, self.max, self.connected, self.idle
        )
    }
}

/// Start the background reaper task that periodically checks for idle agents
pub fn start_reaper(pool: Arc<RwLock<AgentPool>>, check_interval: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(check_interval);
        loop {
            interval.tick().await;
            let mut pool = pool.write().await;
            pool.reap_idle_agents().await;
            let stats = pool.stats();
            if stats.total > 0 {
                debug!("AgentPool stats: {}", stats);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> PoolConfig {
        PoolConfig {
            idle_timeout: Duration::from_secs(2),
            max_agents: 3,
            buffer_messages: true,
            max_buffer_size: 5,
        }
    }

    // ── PoolConfig defaults ──────────────────────────────────────────

    #[test]
    fn pool_config_default_values() {
        let cfg = PoolConfig::default();
        assert_eq!(cfg.idle_timeout, Duration::from_secs(1800));
        assert_eq!(cfg.max_agents, 10);
        assert!(!cfg.buffer_messages);
        assert_eq!(cfg.max_buffer_size, 1000);
    }

    // ── AgentPool::new ───────────────────────────────────────────────

    #[test]
    fn new_pool_is_empty() {
        let pool = AgentPool::new(test_config());
        let stats = pool.stats();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.connected, 0);
        assert_eq!(stats.idle, 0);
        assert_eq!(stats.max, 3);
    }

    // ── get_or_spawn ─────────────────────────────────────────────────

    #[tokio::test]
    async fn spawn_new_agent_with_cat() {
        let mut pool = AgentPool::new(test_config());
        let result = pool.get_or_spawn("token_a", "cat").await;
        assert!(result.is_ok());

        let (_tx, _rx, buffered, was_reused, cached_init) = result.unwrap();
        assert!(!was_reused, "first spawn should not be reused");
        assert!(buffered.is_empty(), "first spawn should have no buffered msgs");
        assert!(cached_init.is_none(), "first spawn should have no cached init");

        let stats = pool.stats();
        assert_eq!(stats.total, 1);
        assert_eq!(stats.connected, 1);

        pool.shutdown_all().await;
    }

    #[tokio::test]
    async fn reuse_existing_agent() {
        let mut pool = AgentPool::new(test_config());

        // First spawn
        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();
        pool.mark_disconnected("token_a");

        // Reconnect
        let (_tx, _rx, _buf, was_reused, _cached) = pool.get_or_spawn("token_a", "cat").await.unwrap();
        assert!(was_reused, "second call should reuse the agent");
        assert_eq!(pool.stats().total, 1);

        pool.shutdown_all().await;
    }

    #[tokio::test]
    async fn spawn_different_tokens() {
        let mut pool = AgentPool::new(test_config());
        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();
        let _ = pool.get_or_spawn("token_b", "cat").await.unwrap();

        assert_eq!(pool.stats().total, 2);
        assert_eq!(pool.stats().connected, 2);

        pool.shutdown_all().await;
    }

    #[tokio::test]
    async fn spawn_with_invalid_command_fails() {
        let mut pool = AgentPool::new(test_config());
        let result = pool.get_or_spawn("token_a", "nonexistent_binary_xyz_42").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn spawn_with_empty_command_fails() {
        let mut pool = AgentPool::new(test_config());
        let result = pool.get_or_spawn("token_a", "").await;
        assert!(result.is_err());
    }

    // ── mark_disconnected / mark_connected ───────────────────────────

    #[tokio::test]
    async fn mark_disconnected_updates_state() {
        let mut pool = AgentPool::new(test_config());
        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();

        assert!(pool.agents.get("token_a").unwrap().connected);

        pool.mark_disconnected("token_a");

        let agent = pool.agents.get("token_a").unwrap();
        assert!(!agent.connected);
        assert!(agent.disconnected_at.is_some());

        let stats = pool.stats();
        assert_eq!(stats.connected, 0);
        assert_eq!(stats.idle, 1);

        pool.shutdown_all().await;
    }

    #[tokio::test]
    async fn reconnect_clears_disconnected_state() {
        let mut pool = AgentPool::new(test_config());
        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();
        pool.mark_disconnected("token_a");

        // Reconnect
        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();
        let agent = pool.agents.get("token_a").unwrap();
        assert!(agent.connected);
        assert!(agent.disconnected_at.is_none());

        pool.shutdown_all().await;
    }

    // ── max-agents limit ─────────────────────────────────────────────

    #[tokio::test]
    async fn max_agents_evicts_idle() {
        let mut pool = AgentPool::new(test_config()); // max_agents = 3

        let _ = pool.get_or_spawn("t1", "cat").await.unwrap();
        let _ = pool.get_or_spawn("t2", "cat").await.unwrap();
        let _ = pool.get_or_spawn("t3", "cat").await.unwrap();
        assert_eq!(pool.stats().total, 3);

        // Disconnect one to make it evictable
        pool.mark_disconnected("t1");

        // 4th spawn should evict the idle agent
        let _ = pool.get_or_spawn("t4", "cat").await.unwrap();
        assert_eq!(pool.stats().total, 3);
        assert!(!pool.agents.contains_key("t1"), "idle agent t1 should be evicted");
    }

    #[tokio::test]
    async fn max_agents_all_connected_fails() {
        let mut pool = AgentPool::new(test_config()); // max_agents = 3

        let _ = pool.get_or_spawn("t1", "cat").await.unwrap();
        let _ = pool.get_or_spawn("t2", "cat").await.unwrap();
        let _ = pool.get_or_spawn("t3", "cat").await.unwrap();

        // All are connected, so 4th should fail
        let result = pool.get_or_spawn("t4", "cat").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Agent pool is full"));

        pool.shutdown_all().await;
    }

    // ── idle timeout / reap ──────────────────────────────────────────

    #[tokio::test]
    async fn reap_removes_timed_out_agents() {
        let cfg = PoolConfig {
            idle_timeout: Duration::from_millis(50),
            max_agents: 10,
            buffer_messages: false,
            max_buffer_size: 100,
        };
        let mut pool = AgentPool::new(cfg);

        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();
        pool.mark_disconnected("token_a");

        // Wait for timeout
        tokio::time::sleep(Duration::from_millis(100)).await;

        pool.reap_idle_agents().await;
        assert_eq!(pool.stats().total, 0, "timed-out agent should be reaped");
    }

    #[tokio::test]
    async fn reap_keeps_connected_agents() {
        let cfg = PoolConfig {
            idle_timeout: Duration::from_millis(50),
            max_agents: 10,
            buffer_messages: false,
            max_buffer_size: 100,
        };
        let mut pool = AgentPool::new(cfg);

        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();
        // Don't disconnect — stays connected

        tokio::time::sleep(Duration::from_millis(100)).await;

        pool.reap_idle_agents().await;
        assert_eq!(pool.stats().total, 1, "connected agent should survive reaping");

        pool.shutdown_all().await;
    }

    #[tokio::test]
    async fn reap_keeps_recently_disconnected() {
        let cfg = PoolConfig {
            idle_timeout: Duration::from_secs(60),
            max_agents: 10,
            buffer_messages: false,
            max_buffer_size: 100,
        };
        let mut pool = AgentPool::new(cfg);

        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();
        pool.mark_disconnected("token_a");

        // Not enough time for timeout
        pool.reap_idle_agents().await;
        assert_eq!(pool.stats().total, 1, "recently-disconnected agent should survive");

        pool.shutdown_all().await;
    }

    // ── message buffering ────────────────────────────────────────────

    #[tokio::test]
    async fn buffer_message_stores_messages() {
        let mut pool = AgentPool::new(test_config()); // buffer_messages = true, max_buffer_size = 5
        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();
        pool.mark_disconnected("token_a");

        pool.buffer_message("token_a", "msg1".into());
        pool.buffer_message("token_a", "msg2".into());

        let agent = pool.agents.get("token_a").unwrap();
        assert_eq!(agent.message_buffer.len(), 2);
        assert_eq!(agent.message_buffer[0], "msg1");
        assert_eq!(agent.message_buffer[1], "msg2");

        pool.shutdown_all().await;
    }

    #[tokio::test]
    async fn buffer_message_respects_max_size() {
        let mut pool = AgentPool::new(test_config()); // max_buffer_size = 5
        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();

        for i in 0..10 {
            pool.buffer_message("token_a", format!("msg{}", i));
        }

        let agent = pool.agents.get("token_a").unwrap();
        assert_eq!(agent.message_buffer.len(), 5, "should cap at max_buffer_size");

        pool.shutdown_all().await;
    }

    #[tokio::test]
    async fn buffer_disabled_drops_messages() {
        let cfg = PoolConfig {
            buffer_messages: false,
            ..test_config()
        };
        let mut pool = AgentPool::new(cfg);
        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();

        pool.buffer_message("token_a", "msg1".into());

        let agent = pool.agents.get("token_a").unwrap();
        assert!(agent.message_buffer.is_empty(), "buffering disabled, should drop");

        pool.shutdown_all().await;
    }

    #[tokio::test]
    async fn reconnect_drains_buffer() {
        let mut pool = AgentPool::new(test_config());
        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();
        pool.mark_disconnected("token_a");

        pool.buffer_message("token_a", "buffered1".into());
        pool.buffer_message("token_a", "buffered2".into());

        // Reconnect — get_or_spawn returns the buffered messages
        let (_tx, _rx, buffered, was_reused, _cached) = pool.get_or_spawn("token_a", "cat").await.unwrap();
        assert!(was_reused);
        assert_eq!(buffered.len(), 2);
        assert_eq!(buffered[0], "buffered1");
        assert_eq!(buffered[1], "buffered2");

        // Buffer should be drained
        let agent = pool.agents.get("token_a").unwrap();
        assert!(agent.message_buffer.is_empty());

        pool.shutdown_all().await;
    }

    // ── remove_agent / shutdown_all ──────────────────────────────────

    #[tokio::test]
    async fn remove_agent_kills_and_removes() {
        let mut pool = AgentPool::new(test_config());
        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();
        assert_eq!(pool.stats().total, 1);

        pool.remove_agent("token_a").await;
        assert_eq!(pool.stats().total, 0);
    }

    #[tokio::test]
    async fn shutdown_all_clears_pool() {
        let mut pool = AgentPool::new(test_config());
        let _ = pool.get_or_spawn("t1", "cat").await.unwrap();
        let _ = pool.get_or_spawn("t2", "cat").await.unwrap();
        let _ = pool.get_or_spawn("t3", "cat").await.unwrap();
        assert_eq!(pool.stats().total, 3);

        pool.shutdown_all().await;
        assert_eq!(pool.stats().total, 0);
    }

    // ── stats ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn stats_reflect_pool_state() {
        let mut pool = AgentPool::new(test_config());
        let _ = pool.get_or_spawn("t1", "cat").await.unwrap();
        let _ = pool.get_or_spawn("t2", "cat").await.unwrap();
        pool.mark_disconnected("t2");

        let s = pool.stats();
        assert_eq!(s.total, 2);
        assert_eq!(s.connected, 1);
        assert_eq!(s.idle, 1);
        assert_eq!(s.max, 3);
        assert!(format!("{}", s).contains("2/3 agents"));

        pool.shutdown_all().await;
    }

    // ── is_alive ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn dead_agent_is_replaced_on_reconnect() {
        let mut pool = AgentPool::new(test_config());
        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();

        // Kill the agent manually
        pool.agents.get_mut("token_a").unwrap().kill().await;
        // Give the process a moment to exit
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Reconnect should spawn fresh
        let (_tx, _rx, _buf, was_reused, _cached) = pool.get_or_spawn("token_a", "cat").await.unwrap();
        assert!(!was_reused, "dead agent should be replaced, not reused");

        pool.shutdown_all().await;
    }

    // ── start_reaper ─────────────────────────────────────────────────

    #[tokio::test]
    async fn reaper_task_cleans_up() {
        let cfg = PoolConfig {
            idle_timeout: Duration::from_millis(50),
            max_agents: 10,
            buffer_messages: false,
            max_buffer_size: 100,
        };
        let pool = Arc::new(RwLock::new(AgentPool::new(cfg)));

        // Spawn and disconnect an agent
        {
            let mut p = pool.write().await;
            let _ = p.get_or_spawn("token_a", "cat").await.unwrap();
            p.mark_disconnected("token_a");
        }

        // Start reaper with short interval
        let handle = start_reaper(Arc::clone(&pool), Duration::from_millis(30));

        // Wait for reaper to run at least once past the idle timeout
        tokio::time::sleep(Duration::from_millis(200)).await;

        let stats = pool.read().await.stats();
        assert_eq!(stats.total, 0, "reaper should have cleaned up the idle agent");

        handle.abort();
    }

    // ── cached initialize response ───────────────────────────────────

    #[tokio::test]
    async fn cache_init_response_stores_and_returns() {
        let mut pool = AgentPool::new(test_config());
        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();

        // No cached response initially
        let agent = pool.agents.get("token_a").unwrap();
        assert!(agent.cached_init_response.is_none());

        // Cache a response
        let fake_init = r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}"#.to_string();
        pool.cache_init_response("token_a", fake_init.clone());

        let agent = pool.agents.get("token_a").unwrap();
        assert_eq!(agent.cached_init_response.as_deref(), Some(fake_init.as_str()));

        // Disconnect and reconnect — cached response should be returned
        pool.mark_disconnected("token_a");
        let (_tx, _rx, _buf, was_reused, cached) = pool.get_or_spawn("token_a", "cat").await.unwrap();
        assert!(was_reused);
        assert_eq!(cached.as_deref(), Some(fake_init.as_str()));

        pool.shutdown_all().await;
    }

    #[tokio::test]
    async fn no_cached_init_for_fresh_spawn() {
        let mut pool = AgentPool::new(test_config());
        let (_tx, _rx, _buf, was_reused, cached) = pool.get_or_spawn("token_a", "cat").await.unwrap();
        assert!(!was_reused);
        assert!(cached.is_none(), "fresh spawn should have no cached init");

        pool.shutdown_all().await;
    }

    #[tokio::test]
    async fn dead_agent_loses_cached_init() {
        let mut pool = AgentPool::new(test_config());
        let _ = pool.get_or_spawn("token_a", "cat").await.unwrap();

        pool.cache_init_response(
            "token_a",
            r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}"#.to_string(),
        );

        // Kill the agent
        pool.agents.get_mut("token_a").unwrap().kill().await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Reconnect — dead agent is replaced, so cached init is gone
        let (_tx, _rx, _buf, was_reused, cached) = pool.get_or_spawn("token_a", "cat").await.unwrap();
        assert!(!was_reused, "dead agent should be replaced");
        assert!(cached.is_none(), "dead agent's cached init should not carry over");

        pool.shutdown_all().await;
    }
}

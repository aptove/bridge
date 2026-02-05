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
    pub async fn get_or_spawn(
        &mut self,
        token: &str,
        agent_command: &str,
    ) -> Result<(mpsc::Sender<String>, broadcast::Receiver<String>, Vec<String>, bool)> {
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

                return Ok((tx, rx, buffered, true));
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
    ) -> Result<(mpsc::Sender<String>, broadcast::Receiver<String>, Vec<String>, bool)> {
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
            agent_command: agent_command.to_string(),
        };

        self.agents.insert(token.to_string(), pooled);

        Ok((ws_to_agent_tx, agent_to_ws_rx, Vec::new(), false))
    }

    /// Mark a client as disconnected. The agent stays alive for idle_timeout.
    pub fn mark_disconnected(&mut self, token: &str) {
        if let Some(agent) = self.agents.get_mut(token) {
            info!("Client disconnected, agent entering idle state (keep-alive)");
            agent.connected = false;
            agent.disconnected_at = Some(Instant::now());
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

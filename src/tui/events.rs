use crossterm::event::KeyEvent;
use crate::common_config::TransportConfig;

/// A single log record captured from the tracing subscriber.
#[derive(Debug, Clone)]
pub struct LogRecord {
    pub timestamp: String,
    pub level: String,
    pub message: String,
}

/// Events emitted by bridge internals → TUI.
#[derive(Debug, Clone)]
pub enum BridgeEvent {
    TransportUp { name: String, addr: String },
    TransportDown { name: String },
    ClientConnected { session_id: String },
    ClientDisconnected { session_id: String },
    PairingCompleted,
    PairingUrlReady { url: String, transport: String },
    AgentSpawned { command: String },
    AgentExited,
    TlsFingerprint { fingerprint: String },
    PushRegistered,
    BridgeStopped,
    BridgeError { message: String },
}

/// Top-level event type that drives the TUI draw loop.
#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    Bridge(BridgeEvent),
    Log(LogRecord),
    Tick,
    Resize(u16, u16),
    /// Result of an async Cloudflare setup triggered from the wizard.
    CloudflareSetupResult(Result<TransportConfig, String>),
    /// Result of an async test-push triggered from the running screen.
    TestPushResult(Result<bool, String>),
}

/// Commands sent from the TUI to the bridge runner.
#[derive(Debug, Clone)]
pub enum TuiCommand {
    Quit,
    Reconnect,
    TestPush,
    RefreshQr,
}

//! ACP Bridge core library — OSS modules.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod agent_pool;
pub mod bridge;
pub mod common_config;
pub mod file_log_layer;
pub mod config;
pub mod pairing;
pub mod push;
pub mod qr;
pub mod rate_limiter;
pub mod runner;
pub mod tailscale;
pub mod tls;
pub mod transport;
pub mod tui;

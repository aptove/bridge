use std::sync::{Arc, atomic::{AtomicU8, Ordering}};
use tokio::sync::mpsc;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::tui::events::{AppEvent, LogRecord};

/// Log level as a u8: ERROR=1  WARN=2  INFO=3  DEBUG=4  TRACE=5.
pub fn level_to_u8(level: Level) -> u8 {
    match level {
        Level::ERROR => 1,
        Level::WARN  => 2,
        Level::INFO  => 3,
        Level::DEBUG => 4,
        Level::TRACE => 5,
    }
}

/// Parse a level name string (case-insensitive) to its u8 value.
/// Returns 2 (WARN) for unrecognised values.
pub fn level_name_to_u8(name: &str) -> u8 {
    match name.to_uppercase().as_str() {
        "ERROR" => 1,
        "WARN"  => 2,
        "INFO"  => 3,
        "DEBUG" => 4,
        "TRACE" => 5,
        _       => 2,
    }
}

/// A `tracing` subscriber layer that forwards log records to the TUI event channel.
///
/// `min_level` is shared with the App so it can be changed at runtime via `/log-level`.
pub struct TuiLogLayer {
    sender: mpsc::Sender<AppEvent>,
    min_level: Arc<AtomicU8>,
}

impl TuiLogLayer {
    pub fn new(sender: mpsc::Sender<AppEvent>, min_level: Arc<AtomicU8>) -> Self {
        Self { sender, min_level }
    }
}

impl<S: Subscriber> Layer<S> for TuiLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = *event.metadata().level();
        // Skip events that are more verbose than the current minimum.
        if level_to_u8(level) > self.min_level.load(Ordering::Relaxed) {
            return;
        }

        let level_str = match level {
            Level::ERROR => "ERROR",
            Level::WARN  => "WARN ",
            Level::INFO  => "INFO ",
            Level::DEBUG => "DEBUG",
            Level::TRACE => "TRACE",
        };

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let now = chrono::Local::now();
        let record = LogRecord {
            timestamp: now.format("%H:%M:%S").to_string(),
            level: level_str.to_string(),
            message: visitor.message,
        };

        // try_send is non-blocking; drop the record if the channel is full.
        let _ = self.sender.try_send(AppEvent::Log(record));
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        }
    }
}

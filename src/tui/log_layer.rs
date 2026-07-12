use tokio::sync::mpsc;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::tui::events::{AppEvent, LogRecord};

/// A `tracing` subscriber layer that forwards log records to the TUI event channel.
pub struct TuiLogLayer {
    sender: mpsc::Sender<AppEvent>,
    min_level: Level,
}

impl TuiLogLayer {
    pub fn new(sender: mpsc::Sender<AppEvent>) -> Self {
        Self { sender, min_level: Level::INFO }
    }

    pub fn with_level(mut self, level: Level) -> Self {
        self.min_level = level;
        self
    }
}

impl<S: Subscriber> Layer<S> for TuiLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = *event.metadata().level();
        if level > self.min_level {
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

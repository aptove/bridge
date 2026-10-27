/// Size-rotating file log layer for tracing.
///
/// Writes all events at DEBUG level and above to
/// `<log_dir>/bridge.log`. When the file reaches `max_bytes`, it is
/// rotated: `bridge.log.{count-1}` is deleted, each `.N` is renamed to
/// `.N+1`, `bridge.log` becomes `bridge.log.1`, and a fresh
/// `bridge.log` is opened. At most `file_count` files are kept on disk.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

pub struct FileLogLayer {
    log_dir: PathBuf,
    max_bytes: u64,
    file_count: usize,
    state: Mutex<FileState>,
}

struct FileState {
    writer: Option<BufWriter<File>>,
    /// Bytes written to the current `bridge.log` since it was opened.
    written: u64,
}

impl FileLogLayer {
    /// Create a new layer. `log_dir` is created if it does not exist.
    ///
    /// - `max_mb`: max size per file in megabytes.
    /// - `file_count`: number of files to keep (including the active one).
    pub fn new(log_dir: PathBuf, max_mb: u64, file_count: usize) -> Self {
        let _ = fs::create_dir_all(&log_dir);

        let log_path = log_dir.join("bridge.log");
        let (writer, written) = open_append(&log_path);

        Self {
            log_dir,
            max_bytes: max_mb.saturating_mul(1024 * 1024),
            file_count: file_count.max(1),
            state: Mutex::new(FileState { writer, written }),
        }
    }

    /// Rotate: delete oldest backup, shift each .N → .N+1, rename
    /// current → .1, then the caller should open a fresh bridge.log.
    fn rotate(&self) {
        // Delete the oldest backup if we are at capacity.
        let oldest = self.log_dir.join(format!("bridge.log.{}", self.file_count - 1));
        if oldest.exists() {
            let _ = fs::remove_file(&oldest);
        }
        // Shift existing backups: .N → .(N+1), starting from the highest.
        for n in (1..self.file_count - 1).rev() {
            let from = self.log_dir.join(format!("bridge.log.{}", n));
            let to   = self.log_dir.join(format!("bridge.log.{}", n + 1));
            if from.exists() {
                let _ = fs::rename(&from, &to);
            }
        }
        // Rename current log to .1.
        let current = self.log_dir.join("bridge.log");
        if current.exists() {
            let _ = fs::rename(&current, self.log_dir.join("bridge.log.1"));
        }
    }
}

fn open_append(path: &PathBuf) -> (Option<BufWriter<File>>, u64) {
    let existing = path.metadata().map(|m| m.len()).unwrap_or(0);
    let writer = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
        .map(BufWriter::new);
    (writer, existing)
}

impl<S: Subscriber> Layer<S> for FileLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        // Write DEBUG and above; skip TRACE to keep file sizes reasonable.
        let level = *event.metadata().level();
        if level == Level::TRACE {
            return;
        }

        let level_str = match level {
            Level::ERROR => "ERROR",
            Level::WARN  => "WARN ",
            Level::INFO  => "INFO ",
            Level::DEBUG => "DEBUG",
            Level::TRACE => "TRACE",
        };

        let mut visitor = MsgVisitor::default();
        event.record(&mut visitor);

        let now = chrono::Local::now();
        let line = format!(
            "{} {} {}\n",
            now.format("%Y-%m-%d %H:%M:%S"),
            level_str,
            visitor.message,
        );
        let line_len = line.len() as u64;

        let Ok(mut st) = self.state.lock() else { return };

        // Rotate if this write would exceed the per-file limit.
        if st.written + line_len > self.max_bytes {
            st.writer = None; // close current file before renaming
            self.rotate();
            let log_path = self.log_dir.join("bridge.log");
            let (w, _) = open_append(&log_path);
            st.writer = w;
            st.written = 0;
        }

        // Lazily (re)open on first use or after a rotation.
        if st.writer.is_none() {
            let log_path = self.log_dir.join("bridge.log");
            let (w, existing) = open_append(&log_path);
            st.writer = w;
            st.written = existing;
        }

        if let Some(ref mut w) = st.writer {
            if w.write_all(line.as_bytes()).is_ok() {
                let _ = w.flush();
                st.written += line_len;
            }
        }
    }
}

#[derive(Default)]
struct MsgVisitor {
    message: String,
}

impl tracing::field::Visit for MsgVisitor {
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

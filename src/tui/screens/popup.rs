use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, List, ListItem},
    Frame,
};

use crate::tui::widgets::qr_popup::{centered_rect, render_qr_popup, render_text_popup};

/// Log level options shown in the picker (name, u8 value).
pub const LOG_LEVELS: &[(&str, u8)] = &[
    ("ERROR", 1),
    ("WARN",  2),
    ("INFO",  3),
    ("DEBUG", 4),
    ("TRACE", 5),
];

#[derive(Debug, Clone, PartialEq)]
pub enum PopupKind {
    QrCode,
    Help,
    /// Interactive log-level picker; `selected` is the highlighted row.
    LogLevel { selected: usize },
}

const HELP_TEXT: &str = "\
/qr           Show QR pairing code
/test-push    Send a test push notification
/reconnect    Restart all transports
/keep-alive   Toggle prevent-sleep (on by default)
/log-level    Change log verbosity
/config       Reconfigure bridge settings
/help         Show this help
/quit         Exit the bridge
";

pub fn render_popup(
    frame: &mut Frame,
    kind: &PopupKind,
    qr_string: &Option<String>,
) {
    match kind {
        PopupKind::QrCode => {
            let qr = qr_string.as_deref().unwrap_or("No QR code available yet.\nStart the bridge first.");
            render_qr_popup(frame, frame.area(), "Pairing QR Code (Esc to close)", qr);
        }
        PopupKind::Help => {
            render_text_popup(frame, frame.area(), "Commands (Esc to close)", HELP_TEXT);
        }
        PopupKind::LogLevel { selected } => {
            render_log_level_popup(frame, *selected);
        }
    }
}

fn render_log_level_popup(frame: &mut Frame, selected: usize) {
    let area = centered_rect(40, 50, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Log Level (↑/↓ select   Enter confirm   Esc cancel) ")
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black).fg(Color::White));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let items: Vec<ListItem> = LOG_LEVELS.iter().enumerate().map(|(i, (name, _))| {
        let label = format!("  {}  ", name);
        let style = if i == selected {
            Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        ListItem::new(label).style(style)
    }).collect();

    // Centre the list vertically inside the block.
    let list_area = Rect {
        y: inner.y + inner.height.saturating_sub(LOG_LEVELS.len() as u16) / 2,
        height: LOG_LEVELS.len() as u16,
        ..inner
    };
    frame.render_widget(List::new(items), list_area);
}

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
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

/// State for the multi-step push-configuration popup.
#[derive(Debug, Clone, PartialEq)]
pub enum PushPopupStep {
    /// Top-level option picker.
    /// `selected` = highlighted row; `active` = currently configured mode (0/1/2).
    Menu { selected: usize, active: usize },
    /// Aptove form: fields = [client_id, client_secret].
    AptoveForm { fields: [String; 2], field_idx: usize, error: Option<String> },
    /// Self-managed form: fields = [push_url, token_url, client_id, client_secret].
    SelfManagedForm { fields: [String; 4], field_idx: usize, error: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PopupKind {
    QrCode,
    Help,
    /// Interactive log-level picker; `selected` is the highlighted row.
    LogLevel { selected: usize },
    /// Push notifications configuration (multi-step).
    PushConfig { step: PushPopupStep },
}

const HELP_TEXT: &str = "\
/qr           Show QR pairing code
/test-push    Send a test push notification
/push         Configure push notifications
/reconnect    Restart the transport
/keep-alive   Toggle prevent-sleep (on by default)
/log-level    Change log verbosity
/clear-logs   Clear the log view
/copy-logs    Copy all logs to clipboard
/agent        Change the AI agent
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
        PopupKind::PushConfig { step } => {
            render_push_popup(frame, step);
        }
    }
}

// ── Push configuration ────────────────────────────────────────────────────────

const PUSH_DOC_URL: &str = "https://doc.aptove.com/bridge/push";
const PUSH_REGISTER_URL: &str = "https://dash.aptove.com";
const PUSH_MENU_LABELS: &[&str] = &[
    "No Push  (buffer messages, no notifications)",
    "Aptove Push Service",
    "Self Managed Push Service",
];

fn render_push_popup(frame: &mut Frame, step: &PushPopupStep) {
    match step {
        PushPopupStep::Menu { selected, active } => render_push_menu(frame, *selected, *active),
        PushPopupStep::AptoveForm { fields, field_idx, error } => {
            render_aptove_form(frame, fields, *field_idx, error.as_deref());
        }
        PushPopupStep::SelfManagedForm { fields, field_idx, error } => {
            render_self_managed_form(frame, fields, *field_idx, error.as_deref());
        }
    }
}

fn render_push_menu(frame: &mut Frame, selected: usize, active: usize) {
    let area = centered_rect(66, 55, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Push Notifications ")
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black).fg(Color::White));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Menu items.
    let items: Vec<ListItem> = PUSH_MENU_LABELS.iter().enumerate().map(|(i, &label)| {
        let prefix = if i == selected { "> " } else { "  " };
        let suffix = if i == active { "  [active]" } else { "" };
        let style = if i == selected {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        ListItem::new(format!("{}{}{}", prefix, label, suffix)).style(style)
    }).collect();

    let list_area = Rect { y: inner.y + 1, height: 3, ..inner };
    frame.render_widget(List::new(items), list_area);

    // Hint.
    let hint_y = inner.y + inner.height.saturating_sub(2);
    frame.render_widget(
        Paragraph::new("↑/↓ navigate   Enter select   Esc cancel")
            .style(Style::default().fg(Color::DarkGray)),
        Rect { y: hint_y, height: 1, ..inner },
    );

    // Footer docs link (clickable).
    let footer_y = inner.y + inner.height.saturating_sub(1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Docs: ", Style::default().fg(Color::DarkGray)),
            Span::styled(PUSH_DOC_URL, Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED)),
        ])),
        Rect { y: footer_y, height: 1, ..inner },
    );
}

fn render_aptove_form(frame: &mut Frame, fields: &[String; 2], field_idx: usize, error: Option<&str>) {
    let area = centered_rect(68, 62, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Aptove Push Service ")
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black).fg(Color::White));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Registration link (clickable).
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Register at: ", Style::default().fg(Color::DarkGray)),
            Span::styled(PUSH_REGISTER_URL, Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED)),
        ])),
        Rect { y: inner.y + 1, height: 1, ..inner },
    );

    // Fields.
    let labels = ["Client ID:", "Client Secret:"];
    for (i, &label) in labels.iter().enumerate() {
        let y = inner.y + 3 + (i as u16) * 2;
        push_field(frame, y, inner.x, inner.width, label, &fields[i], i == field_idx);
    }

    push_footer(frame, inner, error, "Tab next field   Enter confirm   Esc back");
}

fn render_self_managed_form(frame: &mut Frame, fields: &[String; 4], field_idx: usize, error: Option<&str>) {
    let area = centered_rect(72, 72, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Self Managed Push Service ")
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black).fg(Color::White));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Fields.
    let labels = ["Push Service URL:", "Token Service URL:", "Client ID:", "Client Secret:"];
    for (i, &label) in labels.iter().enumerate() {
        let y = inner.y + 1 + (i as u16) * 2;
        push_field(frame, y, inner.x, inner.width, label, &fields[i], i == field_idx);
    }

    push_footer(frame, inner, error, "Tab next field   Enter confirm   Esc back");
}

/// Render a single labelled form field with cursor.
fn push_field(frame: &mut Frame, y: u16, x: u16, width: u16, label: &str, value: &str, active: bool) {
    let label_w = 20u16.min(width / 2);
    let label_style = if active {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    frame.render_widget(
        Paragraph::new(label).style(label_style),
        Rect { x, y, width: label_w, height: 1 },
    );
    let cursor = if active { "█" } else { "" };
    let val_style = if active {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    frame.render_widget(
        Paragraph::new(format!("[{}{}]", value, cursor)).style(val_style),
        Rect { x: x + label_w, y, width: width.saturating_sub(label_w), height: 1 },
    );
}

/// Render hint, optional error, and docs footer inside a form popup.
fn push_footer(frame: &mut Frame, inner: Rect, error: Option<&str>, hint: &str) {
    if let Some(err) = error {
        let y = inner.y + inner.height.saturating_sub(3);
        frame.render_widget(
            Paragraph::new(format!("Error: {}", err)).style(Style::default().fg(Color::Red)),
            Rect { y, height: 1, ..inner },
        );
    }
    let hint_y = inner.y + inner.height.saturating_sub(2);
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
        Rect { y: hint_y, height: 1, ..inner },
    );
    let footer_y = inner.y + inner.height.saturating_sub(1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Docs: ", Style::default().fg(Color::DarkGray)),
            Span::styled(PUSH_DOC_URL, Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED)),
        ])),
        Rect { y: footer_y, height: 1, ..inner },
    );
}

// ── URL hit-testing ───────────────────────────────────────────────────────────

/// Return the static URL string if `(col, row)` lands on a clickable URL inside
/// the current popup, given the terminal's full area.  Returns `None` otherwise.
pub fn url_at(kind: &PopupKind, col: u16, row: u16, term: Rect) -> Option<&'static str> {
    let on_row = |inner: Rect, y: u16| row == y && col >= inner.x && col < inner.x + inner.width;
    let border_inner = |area: Rect| -> Rect {
        // Block with all borders shrinks each side by 1.
        Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        }
    };

    if let PopupKind::PushConfig { step } = kind {
        match step {
            PushPopupStep::Menu { .. } => {
                let inner = border_inner(centered_rect(66, 55, term));
                if on_row(inner, inner.y + inner.height.saturating_sub(1)) {
                    return Some(PUSH_DOC_URL);
                }
            }
            PushPopupStep::AptoveForm { .. } => {
                let inner = border_inner(centered_rect(68, 62, term));
                if on_row(inner, inner.y + 1) {
                    return Some(PUSH_REGISTER_URL);
                }
                if on_row(inner, inner.y + inner.height.saturating_sub(1)) {
                    return Some(PUSH_DOC_URL);
                }
            }
            PushPopupStep::SelfManagedForm { .. } => {
                let inner = border_inner(centered_rect(72, 72, term));
                if on_row(inner, inner.y + inner.height.saturating_sub(1)) {
                    return Some(PUSH_DOC_URL);
                }
            }
        }
    }
    None
}

// ── Log level picker ──────────────────────────────────────────────────────────

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

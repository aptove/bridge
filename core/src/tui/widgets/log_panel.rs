use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
    Frame,
};

use crate::tui::events::LogRecord;

pub fn render_log_panel(
    frame: &mut Frame,
    area: Rect,
    logs: &[LogRecord],
    scroll_offset: usize,
    log_highlight: Option<(u16, u16)>,
    copy_hint: Option<&str>,
) {
    // Top row: scroll indicator (empty when at tail, text when scrolled up).
    let indicator_area = Rect { height: 1, ..area };
    let log_area = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    let visible_height = log_area.height as usize;

    // Clamp offset and compute the first visible log index.
    let total = logs.len();
    let (start, clamped_offset) = if total > visible_height && visible_height > 0 {
        let max_offset = total - visible_height;
        let offset = scroll_offset.min(max_offset);
        (max_offset - offset, offset)
    } else {
        (0, 0)
    };

    // Render indicator row: copy hint takes priority over scroll info.
    let (indicator_text, indicator_style) = if let Some(hint) = copy_hint {
        (hint.to_string(), Style::default().fg(Color::Green))
    } else if clamped_offset > 0 {
        (
            format!(" ↑ {} lines from bottom  (↓ / PgDn to resume)", clamped_offset),
            Style::default().fg(Color::DarkGray),
        )
    } else {
        (String::new(), Style::default())
    };
    frame.render_widget(
        Paragraph::new(indicator_text).style(indicator_style),
        indicator_area,
    );

    // Convert the terminal-row highlight range to visual indices within the log area.
    let highlight_visual: Option<(usize, usize)> = log_highlight.and_then(|(top, bot)| {
        if bot < log_area.y || top >= log_area.y + log_area.height {
            return None;
        }
        let first = top.saturating_sub(log_area.y) as usize;
        let last = ((bot - log_area.y) as usize).min(visible_height.saturating_sub(1));
        Some((first, last))
    });

    // Render log lines — no borders, no block.
    let end = (start + visible_height).min(total);
    let items: Vec<ListItem> = logs[start..end]
        .iter()
        .enumerate()
        .map(|(visual_idx, r)| {
            let level_style = match r.level.trim() {
                "ERROR" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                "WARN"  => Style::default().fg(Color::Yellow),
                "INFO"  => Style::default().fg(Color::Cyan),
                _       => Style::default().fg(Color::DarkGray),
            };
            let line = Line::from(vec![
                Span::styled(format!("{} ", r.timestamp), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{} ", r.level), level_style),
                Span::raw(r.message.clone()),
            ]);
            let is_highlighted = highlight_visual
                .map(|(first, last)| visual_idx >= first && visual_idx <= last)
                .unwrap_or(false);
            if is_highlighted {
                ListItem::new(line).style(Style::default().bg(Color::DarkGray))
            } else {
                ListItem::new(line)
            }
        })
        .collect();

    frame.render_widget(List::new(items), log_area);
}

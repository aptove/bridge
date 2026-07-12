use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem},
    Frame,
};

use crate::tui::events::LogRecord;

pub fn render_log_panel(
    frame: &mut Frame,
    area: Rect,
    logs: &[LogRecord],
    scroll_offset: usize,
) {
    let visible_height = area.height.saturating_sub(2) as usize; // minus borders

    // scroll_offset = 0 means show the tail; larger = scrolled up.
    let total = logs.len();
    let start = if total > visible_height {
        let max_start = total - visible_height;
        max_start.saturating_sub(scroll_offset)
    } else {
        0
    };

    let items: Vec<ListItem> = logs[start..]
        .iter()
        .map(|r| {
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
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Log"));

    frame.render_widget(list, area);
}

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::Paragraph,
    Frame,
};

pub struct TransportState {
    pub name: String,
    pub addr: String,
    pub up: bool,
}

pub fn render_status_bar(
    frame: &mut Frame,
    area: Rect,
    version: &str,
    transports: &[TransportState],
    push_up: bool,
) {
    let mut parts = vec![format!(" Aptove Bridge v{}", version)];

    for t in transports {
        let icon = if t.up { "◉" } else { "○" };
        parts.push(format!("  [{} {}]", t.name, icon));
    }

    if push_up {
        parts.push("  [push ◉]".to_string());
    }

    let text = parts.join("");
    let para = Paragraph::new(text)
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
    frame.render_widget(para, area);
}

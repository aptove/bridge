use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
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
    keep_alive: bool,
) {
    let base = Style::default().bg(Color::DarkGray).fg(Color::White);
    let mut spans: Vec<Span> = vec![Span::styled(
        format!(" Aptove Bridge v{}", version),
        base,
    )];

    for t in transports {
        let dot_color = if t.up { Color::Green } else { Color::Red };
        spans.push(Span::styled(format!("  [{} ", t.name), base));
        spans.push(Span::styled("●", Style::default().bg(Color::DarkGray).fg(dot_color)));
        spans.push(Span::styled("]", base));
    }

    if push_up {
        spans.push(Span::styled("  [push ", base));
        spans.push(Span::styled("●", Style::default().bg(Color::DarkGray).fg(Color::Green)));
        spans.push(Span::styled("]", base));
    }

    let ka_color = if keep_alive { Color::Green } else { Color::Red };
    spans.push(Span::styled("  [keep alive ", base));
    spans.push(Span::styled("●", Style::default().bg(Color::DarkGray).fg(ka_color)));
    spans.push(Span::styled("]", base));

    let para = Paragraph::new(Line::from(spans)).style(base);
    frame.render_widget(para, area);
}

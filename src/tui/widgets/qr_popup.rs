use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::Text,
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

/// Returns a centered `Rect` of the given percentage of the outer `Rect`.
pub fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Render a QR code popup in the center of the screen.
pub fn render_qr_popup(frame: &mut Frame, area: Rect, title: &str, qr_string: &str) {
    let popup_area = centered_rect(70, 80, area);

    // Clear the background.
    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(format!(" {} ", title))
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black).fg(Color::White));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let text = Text::raw(qr_string);
    let para = Paragraph::new(text)
        .style(Style::default().bg(Color::Black).fg(Color::White));
    frame.render_widget(para, inner);
}

/// Render a generic text popup in the center of the screen.
pub fn render_text_popup(frame: &mut Frame, area: Rect, title: &str, content: &str) {
    let popup_area = centered_rect(60, 60, area);
    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(format!(" {} ", title))
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black).fg(Color::White));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let para = Paragraph::new(content)
        .style(Style::default().bg(Color::Black).fg(Color::White));
    frame.render_widget(para, inner);
}

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

/// A single autocomplete suggestion.
pub struct AcEntry<'a> {
    pub command: &'a str,
    pub description: &'a str,
}

/// Autocomplete state passed from the app to the renderer.
pub struct AutocompleteState<'a> {
    pub matches: &'a [AcEntry<'a>],
    pub selected: usize,
}

pub fn render_input_bar(
    frame: &mut Frame,
    area: Rect,
    input: &str,
    hint: &str,
    autocomplete: Option<&AutocompleteState<'_>>,
) {
    // Autocomplete dropdown (rendered above the input bar).
    if let Some(ac) = autocomplete {
        if !ac.matches.is_empty() {
            render_dropdown(frame, area, ac);
        }
    }

    // Input line.
    let display = if input.is_empty() {
        Line::from(vec![
            Span::raw("> "),
            Span::styled(hint, Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(vec![
            Span::raw("> "),
            Span::raw(input),
            Span::styled("█", Style::default().fg(Color::White)),
        ])
    };

    let para = Paragraph::new(display)
        .block(Block::default().borders(Borders::TOP));
    frame.render_widget(para, area);
}

fn render_dropdown(frame: &mut Frame, input_area: Rect, ac: &AutocompleteState<'_>) {
    let count = ac.matches.len() as u16;
    let dropdown_height = count + 2; // borders

    // Float upward from the top edge of the input bar.
    let y = input_area.y.saturating_sub(dropdown_height);
    let width = input_area.width.min(52);
    let dropdown_area = Rect { x: input_area.x + 1, y, width, height: dropdown_height };

    frame.render_widget(Clear, dropdown_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
    let inner = block.inner(dropdown_area);
    frame.render_widget(block, dropdown_area);

    let items: Vec<ListItem> = ac.matches.iter().enumerate().map(|(i, entry)| {
        let text = format!("{:<16} {}", entry.command, entry.description);
        let style = if i == ac.selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White).bg(Color::DarkGray)
        };
        ListItem::new(text).style(style)
    }).collect();

    frame.render_widget(List::new(items), inner);
}

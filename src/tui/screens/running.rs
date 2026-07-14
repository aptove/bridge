use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

use crate::tui::{
    events::LogRecord,
    widgets::{
        input_bar::{render_input_bar, AutocompleteState},
        log_panel::render_log_panel,
        status_bar::{render_status_bar, TransportState},
    },
};

pub struct RunningState<'a> {
    pub transport_name: String,
    pub transport_addr: String,
    pub transport_up: bool,
    pub push_up: bool,
    pub keep_alive: bool,
    /// Brief message shown in the log indicator row (e.g. "✓ Copied!").
    pub copy_hint: Option<&'a str>,
}

pub fn render_running(
    frame: &mut Frame,
    state: &RunningState<'_>,
    logs: &[LogRecord],
    log_scroll: usize,
    input: &str,
    version: &str,
    autocomplete: Option<&AutocompleteState<'_>>,
) {
    let area = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),    // log panel
            Constraint::Length(3), // input bar
            Constraint::Length(1), // status bar
        ])
        .split(area);

    let transports = vec![TransportState {
        name: state.transport_name.clone(),
        addr: state.transport_addr.clone(),
        up: state.transport_up,
    }];
    render_log_panel(frame, chunks[0], logs, log_scroll, state.copy_hint);
    render_input_bar(frame, chunks[1], input, "type /help for commands", autocomplete);
    render_status_bar(frame, chunks[2], version, &transports, state.push_up, state.keep_alive);
}

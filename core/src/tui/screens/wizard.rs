use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use crate::common_config::CommonConfig;
use crate::tailscale::{is_tailscale_available, is_tailscale_installed};
use crate::transport;

pub const AGENTS: &[(&str, &str)] = &[
    ("GitHub Copilot",           "copilot --acp"),
    ("Google Gemini",            "gemini --experimental-acp"),
    ("Goose AI",                 "goose acp"),
    ("Claude Code (Claude CLI)", "claude-acp"),
    ("Custom...",                ""),
];

/// All possible wizard steps.
#[derive(Debug, Clone)]
pub enum WizardStep {
    /// Agent selection menu.
    AgentSelect { selected: usize },
    /// Custom agent command text input.
    AgentCustomInput { input: String },
    /// Pick which transport to use.
    TransportPick {
        selected: usize,
        ts_available: bool,
        ts_installed: bool,
        /// Status badge per transport slot (parallel to `transport::all()`).
        statuses: Vec<String>,
    },
    /// Generic plugin wizard form — used when a transport's `wizard_fields()`
    /// returns `Some(_)` (e.g. Cloudflare Zero Trust in enterprise builds).
    PluginSetup {
        transport_name: String,
        fields: Vec<String>,
        field_idx: usize,
        error: Option<String>,
    },
    /// Shown while the plugin's `run_setup()` future is in progress.
    PluginLoading { transport_name: String },
    /// Push notification setup form (optional).
    PushSetup {
        fields: [String; 4], // token_url, push_url, client_id, client_secret
        field_idx: usize,
        error: Option<String>,
    },
    /// All config complete.
    Done,
}

/// The full wizard state.
pub struct WizardState {
    pub step: WizardStep,
    pub reconnect_mode: bool,
    pub cancelable: bool,
}

impl WizardState {
    pub fn compute(config: &CommonConfig) -> Option<Self> {
        if config.agent_command.is_none() {
            return Some(Self { step: WizardStep::AgentSelect { selected: 0 }, reconnect_mode: false, cancelable: false });
        }

        let enabled_count = config.enabled_transports().len();
        if enabled_count != 1 {
            let ts_available = is_tailscale_available();
            let ts_installed = is_tailscale_installed();
            let statuses = compute_transport_statuses(config, None, ts_available, ts_installed);
            return Some(Self {
                step: WizardStep::TransportPick { selected: 0, ts_available, ts_installed, statuses },
                reconnect_mode: false,
                cancelable: false,
            });
        }

        // Single transport — check if any plugin reports it needs wizard setup.
        let enabled_name = config.enabled_transports()[0].0;
        if let Some(plugin) = transport::get(enabled_name) {
            if !plugin.is_configured(config) {
                if let Some(fields) = plugin.wizard_fields() {
                    let defaults: Vec<String> = fields.iter().map(|f| f.default_value.to_string()).collect();
                    return Some(Self {
                        step: WizardStep::PluginSetup {
                            transport_name: enabled_name.to_string(),
                            fields: defaults,
                            field_idx: 0,
                            error: None,
                        },
                        reconnect_mode: false,
                        cancelable: false,
                    });
                }
            }
        }

        None
    }

    pub fn for_reconnect(config: &CommonConfig, active_transport: Option<&str>) -> Self {
        let ts_available = is_tailscale_available();
        let ts_installed = is_tailscale_installed();
        let statuses = compute_transport_statuses(config, active_transport, ts_available, ts_installed);
        Self {
            step: WizardStep::TransportPick { selected: 0, ts_available, ts_installed, statuses },
            reconnect_mode: true,
            cancelable: true,
        }
    }
}

/// Compute status badge strings for all registered transports.
pub fn compute_transport_statuses(
    config: &CommonConfig,
    active: Option<&str>,
    ts_available: bool,
    ts_installed: bool,
) -> Vec<String> {
    transport::all().iter().map(|plugin| {
        let name = plugin.name();
        let is_enabled = config.transports.get(name).map(|t| t.enabled).unwrap_or(false);
        let is_ready = is_enabled && plugin.is_configured(config);

        if is_ready {
            if active == Some(name) { "[active]".to_string() }
            else { "[ready]".to_string() }
        } else {
            match name {
                "local" => "[auto-configure]".to_string(),
                "tailscale-serve" => {
                    if ts_available { "[available]".to_string() }
                    else if ts_installed { "[not running]".to_string() }
                    else { "[not installed]".to_string() }
                }
                _ if plugin.wizard_fields().is_some() => "[setup required]".to_string(),
                _ => "[auto-configure]".to_string(),
            }
        }
    }).collect()
}

// ── Input helpers ─────────────────────────────────────────────────────────────

pub fn wizard_type_char(state: &mut WizardState, c: char) {
    match &mut state.step {
        WizardStep::AgentCustomInput { input } => input.push(c),
        WizardStep::PluginSetup { fields, field_idx, .. } => {
            fields[*field_idx].push(c);
        }
        WizardStep::PushSetup { fields, field_idx, .. } => {
            fields[*field_idx].push(c);
        }
        _ => {}
    }
}

pub fn wizard_backspace(state: &mut WizardState) {
    match &mut state.step {
        WizardStep::AgentCustomInput { input } => { input.pop(); }
        WizardStep::PluginSetup { fields, field_idx, .. } => {
            fields[*field_idx].pop();
        }
        WizardStep::PushSetup { fields, field_idx, .. } => {
            fields[*field_idx].pop();
        }
        _ => {}
    }
}

pub fn wizard_next_field(state: &mut WizardState) {
    match &mut state.step {
        WizardStep::PluginSetup { fields, field_idx, .. } => {
            *field_idx = (*field_idx + 1) % fields.len().max(1);
        }
        WizardStep::PushSetup { field_idx, .. } => {
            *field_idx = (*field_idx + 1) % 4;
        }
        _ => {}
    }
}

pub fn wizard_move_up(state: &mut WizardState) {
    match &mut state.step {
        WizardStep::AgentSelect { selected } => {
            *selected = selected.saturating_sub(1);
        }
        WizardStep::TransportPick { selected, .. } => {
            *selected = selected.saturating_sub(1);
        }
        _ => {}
    }
}

pub fn wizard_move_down(state: &mut WizardState) {
    match &mut state.step {
        WizardStep::AgentSelect { selected } => {
            *selected = (*selected + 1).min(AGENTS.len() - 1);
        }
        WizardStep::TransportPick { selected, .. } => {
            let count = transport::all().len();
            *selected = (*selected + 1).min(count.saturating_sub(1));
        }
        _ => {}
    }
}

pub fn wizard_confirm_agent(state: &WizardState) -> Option<Option<String>> {
    if let WizardStep::AgentSelect { selected } = &state.step {
        let (_, cmd) = AGENTS[*selected];
        if cmd.is_empty() { None } else { Some(Some(cmd.to_string())) }
    } else {
        None
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

pub fn render_wizard(frame: &mut Frame, state: &WizardState) {
    match &state.step {
        WizardStep::AgentSelect { selected } => {
            let hint = if state.cancelable {
                "↑/↓ navigate   Enter confirm   Esc cancel"
            } else {
                "↑/↓ navigate   Enter confirm"
            };
            let inner = wizard_panel(frame, "Select Agent", hint);
            let items: Vec<ListItem> = AGENTS.iter().enumerate().map(|(i, (name, cmd))| {
                let prefix = if i == *selected { "> " } else { "  " };
                let label = if cmd.is_empty() {
                    format!("{}{}", prefix, name)
                } else {
                    format!("{}{}  ({})", prefix, name, cmd)
                };
                ListItem::new(label).style(if i == *selected {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                })
            }).collect();
            let list_area = Rect { y: inner.y + 1, height: inner.height.saturating_sub(2), ..inner };
            frame.render_widget(List::new(items), list_area);
        }

        WizardStep::AgentCustomInput { input } => {
            let inner = wizard_panel(frame, "Custom Agent Command", "Enter confirm   Esc back");
            let text = format!("Command: [{}█]", input);
            let p_area = Rect { y: inner.y + 2, height: 3, ..inner };
            frame.render_widget(
                Paragraph::new(text).style(Style::default().fg(Color::White)),
                p_area,
            );
        }

        WizardStep::TransportPick { selected, statuses, .. } => {
            let title = if state.reconnect_mode {
                "Choose Transport to Reconnect"
            } else {
                "Choose Transport for This Session"
            };
            let pick_hint = if state.cancelable {
                "↑/↓ navigate   Enter select   Esc cancel"
            } else {
                "↑/↓ navigate   Enter select   unconfigured → inline setup"
            };
            let inner = wizard_panel(frame, title, pick_hint);
            let plugins = transport::all();
            let items: Vec<ListItem> = plugins.iter().enumerate().map(|(i, plugin)| {
                let prefix = if i == *selected { "> " } else { "  " };
                let status = statuses.get(i).map(|s| s.as_str()).unwrap_or("");
                let text = format!("{}{:<30} {}", prefix, plugin.display_name(), status);
                ListItem::new(text).style(if i == *selected {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                })
            }).collect();
            let list_area = Rect { y: inner.y + 1, height: inner.height.saturating_sub(2), ..inner };
            frame.render_widget(List::new(items), list_area);
        }

        WizardStep::PluginSetup { transport_name, fields, field_idx, error } => {
            let plugin = transport::get(transport_name.as_str());
            let title = plugin.map(|p| p.display_name()).unwrap_or("Transport Setup");
            let inner = wizard_panel(
                frame,
                &format!("{} Setup", title),
                "Tab next field   Enter submit   Esc back",
            );
            let wizard_fields = plugin.and_then(|p| p.wizard_fields()).unwrap_or_default();
            let labels: Vec<&str> = wizard_fields.iter().map(|f| f.label).collect();
            render_form_dynamic(frame, inner, &labels, fields, *field_idx, error.as_deref());
        }

        WizardStep::PluginLoading { transport_name } => {
            let plugin = transport::get(transport_name.as_str());
            let title = plugin.map(|p| p.display_name()).unwrap_or("Transport");
            let inner = wizard_panel(frame, &format!("{} Setup", title), "");
            let p_area = Rect { y: inner.y + 2, height: 2, ..inner };
            frame.render_widget(
                Paragraph::new(format!("Configuring {}...", title))
                    .style(Style::default().fg(Color::Yellow)),
                p_area,
            );
        }

        WizardStep::PushSetup { fields, field_idx, error } => {
            let labels = ["Token service URL:", "Push service URL:", "Client ID:", "Client Secret:"];
            let values: Vec<String> = fields.to_vec();
            let inner = wizard_panel(
                frame,
                "Push Notifications (optional)",
                "Tab next field   Enter submit   Esc skip",
            );
            render_form_dynamic(frame, inner, &labels, &values, *field_idx, error.as_deref());
        }

        WizardStep::Done => {
            let inner = wizard_panel(frame, "Setup Complete", "");
            let p_area = Rect { y: inner.y + 2, height: 2, ..inner };
            frame.render_widget(
                Paragraph::new("Starting bridge...")
                    .style(Style::default().fg(Color::Green)),
                p_area,
            );
        }
    }
}

fn wizard_panel(frame: &mut Frame, title: &str, hint: &str) -> Rect {
    let area = frame.area();
    let popup = centered(60, 70, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(format!(" {} ", title))
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black).fg(Color::White));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if !hint.is_empty() {
        let hint_area = Rect {
            y: inner.y + inner.height.saturating_sub(1),
            height: 1,
            ..inner
        };
        frame.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray))),
            hint_area,
        );
    }
    inner
}

fn render_form_dynamic(
    frame: &mut Frame,
    area: Rect,
    labels: &[&str],
    values: &[String],
    active: usize,
    error: Option<&str>,
) {
    let max_label = labels.iter().map(|l| l.len()).max().unwrap_or(0);

    for (i, label) in labels.iter().enumerate() {
        let y = area.y + 1 + (i as u16) * 2;
        if y >= area.y + area.height { break; }

        let label_w = (max_label + 2) as u16;
        let label_area = Rect { x: area.x, y, width: label_w, height: 1 };
        let value_area = Rect {
            x: area.x + label_w,
            y,
            width: area.width.saturating_sub(label_w),
            height: 1,
        };

        let label_style = if i == active {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        frame.render_widget(Paragraph::new(*label).style(label_style), label_area);

        let val = values.get(i).map(|s| s.as_str()).unwrap_or("");
        let cursor = if i == active { "█" } else { "" };
        let val_text = format!("[{}{}]", val, cursor);
        let val_style = if i == active {
            Style::default().fg(Color::White)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        frame.render_widget(Paragraph::new(val_text).style(val_style), value_area);
    }

    if let Some(err) = error {
        let y = area.y + area.height.saturating_sub(2);
        let err_area = Rect { y, height: 1, ..area };
        frame.render_widget(
            Paragraph::new(format!("Error: {}", err))
                .style(Style::default().fg(Color::Red)),
            err_area,
        );
    }
}

fn centered(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
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
        .split(vertical[1])[1]
}

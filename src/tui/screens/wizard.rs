use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use crate::common_config::CommonConfig;
use crate::tailscale::{is_tailscale_available, is_tailscale_installed};

pub const AGENTS: &[(&str, &str)] = &[
    ("GitHub Copilot", "copilot --acp"),
    ("Google Gemini",  "gemini --experimental-acp"),
    ("Goose AI",       "goose acp"),
    ("Custom...",      ""),
];

const TRANSPORTS: &[&str] = &["local", "tailscale-serve", "cloudflare"];
const TRANSPORT_LABELS: &[&str] = &["Local Bridge Server", "Tailscale (Recommended)", "Cloudflare Zero Trust"];

/// All possible wizard steps.
#[derive(Debug, Clone)]
pub enum WizardStep {
    /// Agent selection menu.
    AgentSelect { selected: usize },
    /// Custom agent command text input (shown after selecting "Custom...").
    AgentCustomInput { input: String },
    /// Transport selection menu.
    TransportSelect { selected: usize, ts_available: bool, ts_installed: bool },
    /// Cloudflare Zero Trust form (field_idx: current active field).
    CloudflareSetup {
        fields: [String; 4], // api_token, account_id, domain, subdomain
        field_idx: usize,
        error: Option<String>,
    },
    /// Shown while the async Cloudflare API calls are in progress.
    CloudflareLoading,
    /// Push notification setup form (optional, Esc to skip).
    PushSetup {
        fields: [String; 4], // token_url, push_url, client_id, client_secret
        field_idx: usize,
        error: Option<String>,
    },
    /// All config complete.
    Done,
}

impl WizardStep {
    pub fn is_done(&self) -> bool {
        matches!(self, WizardStep::Done)
    }
}

/// The full wizard state, including current step and in-progress text edits.
pub struct WizardState {
    pub step: WizardStep,
}

impl WizardState {
    /// Compute the first wizard step needed based on current config.
    /// Returns `None` if no wizard is needed (config is complete).
    pub fn compute(config: &CommonConfig) -> Option<Self> {
        // 1. Agent command missing?
        if config.agent_command.is_none() {
            return Some(Self { step: WizardStep::AgentSelect { selected: 0 } });
        }

        // 2. No enabled transport?
        if config.enabled_transports().is_empty() {
            let ts_available = is_tailscale_available();
            let ts_installed = is_tailscale_installed();
            return Some(Self {
                step: WizardStep::TransportSelect { selected: 0, ts_available, ts_installed },
            });
        }

        // 3. Cloudflare transport present but unconfigured?
        if let Some(cf) = config.transports.get("cloudflare") {
            if cf.enabled && cf.tunnel_id.is_none() {
                return Some(Self {
                    step: WizardStep::CloudflareSetup {
                        fields: [
                            String::new(),
                            String::new(),
                            String::new(),
                            "agent".to_string(),
                        ],
                        field_idx: 0,
                        error: None,
                    },
                });
            }
        }

        // 4. Push not configured?
        if config.push_relay.is_none() {
            return Some(Self {
                step: WizardStep::PushSetup {
                    fields: [
                        "https://token.aptove.com".to_string(),
                        "https://push.aptove.com".to_string(),
                        String::new(),
                        String::new(),
                    ],
                    field_idx: 0,
                    error: None,
                },
            });
        }

        None
    }
}

/// Handle a character typed in a text-input wizard step.
pub fn wizard_type_char(state: &mut WizardState, c: char) {
    match &mut state.step {
        WizardStep::AgentCustomInput { input } => input.push(c),
        WizardStep::CloudflareSetup { fields, field_idx, .. } => {
            fields[*field_idx].push(c);
        }
        WizardStep::PushSetup { fields, field_idx, .. } => {
            fields[*field_idx].push(c);
        }
        _ => {}
    }
}

/// Handle backspace in a text-input wizard step.
pub fn wizard_backspace(state: &mut WizardState) {
    match &mut state.step {
        WizardStep::AgentCustomInput { input } => { input.pop(); }
        WizardStep::CloudflareSetup { fields, field_idx, .. } => {
            fields[*field_idx].pop();
        }
        WizardStep::PushSetup { fields, field_idx, .. } => {
            fields[*field_idx].pop();
        }
        _ => {}
    }
}

/// Handle Tab (next field) in form steps.
pub fn wizard_next_field(state: &mut WizardState) {
    match &mut state.step {
        WizardStep::CloudflareSetup { field_idx, .. } => {
            *field_idx = (*field_idx + 1) % 4;
        }
        WizardStep::PushSetup { field_idx, .. } => {
            *field_idx = (*field_idx + 1) % 4;
        }
        _ => {}
    }
}

/// Handle up arrow in menu steps.
pub fn wizard_move_up(state: &mut WizardState) {
    match &mut state.step {
        WizardStep::AgentSelect { selected } => {
            *selected = selected.saturating_sub(1);
        }
        WizardStep::TransportSelect { selected, .. } => {
            *selected = selected.saturating_sub(1);
        }
        _ => {}
    }
}

/// Handle down arrow in menu steps.
pub fn wizard_move_down(state: &mut WizardState) {
    match &mut state.step {
        WizardStep::AgentSelect { selected } => {
            *selected = (*selected + 1).min(AGENTS.len() - 1);
        }
        WizardStep::TransportSelect { selected, .. } => {
            *selected = (*selected + 1).min(TRANSPORTS.len() - 1);
        }
        _ => {}
    }
}

/// Returns the selected agent command when the user confirms agent selection.
/// Returns `None` if they chose "Custom..." (transition to custom input).
pub fn wizard_confirm_agent(state: &WizardState) -> Option<Option<String>> {
    if let WizardStep::AgentSelect { selected } = &state.step {
        let (_, cmd) = AGENTS[*selected];
        if cmd.is_empty() {
            // Custom selected — need text input
            None
        } else {
            Some(Some(cmd.to_string()))
        }
    } else {
        None
    }
}

/// Called when the user confirms a transport selection.
/// Returns `(internal_name, needs_cf_setup)`.
pub fn wizard_confirm_transport(state: &WizardState) -> Option<(&'static str, bool)> {
    if let WizardStep::TransportSelect { selected, .. } = &state.step {
        let name = TRANSPORTS[*selected];
        let needs_cf = name == "cloudflare";
        Some((name, needs_cf))
    } else {
        None
    }
}

// ── Rendering ───────────────────────────────────────────────────────────────

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

    // Hint at the bottom
    if !hint.is_empty() {
        let hint_area = Rect {
            y: inner.y + inner.height.saturating_sub(1),
            height: 1,
            ..inner
        };
        let para = Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray)));
        frame.render_widget(para, hint_area);
    }
    inner
}

pub fn render_wizard(frame: &mut Frame, state: &WizardState) {
    match &state.step {
        WizardStep::AgentSelect { selected } => {
            let inner = wizard_panel(frame, "Select Agent", "↑/↓ navigate   Enter confirm");
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
            let list = List::new(items);
            let list_area = Rect { y: inner.y + 1, height: inner.height.saturating_sub(2), ..inner };
            frame.render_widget(list, list_area);
        }

        WizardStep::AgentCustomInput { input } => {
            let inner = wizard_panel(frame, "Custom Agent Command", "Enter confirm   Esc back");
            let text = format!("Command: [{}█]", input);
            let para = Paragraph::new(text).style(Style::default().fg(Color::White));
            let p_area = Rect { y: inner.y + 2, height: 3, ..inner };
            frame.render_widget(para, p_area);
        }

        WizardStep::TransportSelect { selected, ts_available, ts_installed, .. } => {
            let inner = wizard_panel(frame, "Select Transport", "↑/↓ navigate   Enter confirm");
            let items: Vec<ListItem> = TRANSPORTS.iter().enumerate().map(|(i, &name)| {
                let prefix = if i == *selected { "> " } else { "  " };
                let label_str = TRANSPORT_LABELS[i];
                let status = match name {
                    "local" => "[auto-configure]",
                    "tailscale-serve" => {
                        if *ts_available { "[running]" }
                        else if *ts_installed { "[not running]" }
                        else { "[not installed]" }
                    }
                    "cloudflare" => "[setup required]",
                    _ => "",
                };
                let text = format!("{}{:<30} {}", prefix, label_str, status);
                ListItem::new(text).style(if i == *selected {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                })
            }).collect();
            let list = List::new(items);
            let list_area = Rect { y: inner.y + 1, height: inner.height.saturating_sub(2), ..inner };
            frame.render_widget(list, list_area);
        }

        WizardStep::CloudflareSetup { fields, field_idx, error } => {
            let inner = wizard_panel(frame, "Cloudflare Zero Trust Setup", "Tab next field   Enter submit   Esc back");
            render_form(frame, inner, &[
                "API Token:",
                "Account ID:",
                "Domain (e.g. example.com):",
                "Subdomain [agent]:",
            ], fields, *field_idx, error.as_deref());
        }

        WizardStep::CloudflareLoading => {
            let inner = wizard_panel(frame, "Cloudflare Setup", "");
            let para = Paragraph::new("Configuring Cloudflare Zero Trust...")
                .style(Style::default().fg(Color::Yellow));
            frame.render_widget(para, Rect { y: inner.y + 2, height: 2, ..inner });
        }

        WizardStep::PushSetup { fields, field_idx, error } => {
            let inner = wizard_panel(frame, "Push Notifications (optional)", "Tab next field   Enter submit   Esc skip");
            render_form(frame, inner, &[
                "Token service URL:",
                "Push service URL:",
                "Client ID:",
                "Client Secret:",
            ], fields, *field_idx, error.as_deref());
        }

        WizardStep::Done => {
            let inner = wizard_panel(frame, "Setup Complete", "");
            let para = Paragraph::new("Starting bridge...")
                .style(Style::default().fg(Color::Green));
            frame.render_widget(para, Rect { y: inner.y + 2, height: 2, ..inner });
        }
    }
}

fn render_form(
    frame: &mut Frame,
    area: Rect,
    labels: &[&str],
    values: &[String; 4],
    active: usize,
    error: Option<&str>,
) {
    let max_label = labels.iter().map(|l| l.len()).max().unwrap_or(0);

    for (i, label) in labels.iter().enumerate() {
        let y = area.y + 1 + (i as u16) * 2;
        if y >= area.y + area.height { break; }

        let label_area = Rect { x: area.x, y, width: (max_label + 2) as u16, height: 1 };
        let value_area = Rect {
            x: area.x + (max_label + 2) as u16,
            y,
            width: area.width.saturating_sub((max_label + 2) as u16),
            height: 1,
        };

        let label_style = if i == active {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        frame.render_widget(Paragraph::new(*label).style(label_style), label_area);

        let cursor = if i == active { "█" } else { "" };
        let val_text = format!("[{}{}]", values[i], cursor);
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

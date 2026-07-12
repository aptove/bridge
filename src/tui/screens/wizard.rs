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

pub const TRANSPORTS: &[&str] = &["local", "tailscale-serve", "cloudflare"];
const TRANSPORT_LABELS: &[&str] = &["Local Bridge Server", "Tailscale (Recommended)", "Cloudflare Zero Trust"];

/// All possible wizard steps.
#[derive(Debug, Clone)]
pub enum WizardStep {
    /// Agent selection menu.
    AgentSelect { selected: usize },
    /// Custom agent command text input (shown after selecting "Custom...").
    AgentCustomInput { input: String },
    /// Pick which transport to use for this session.
    ///
    /// Shown when:
    ///   • 0 enabled transports (first run)
    ///   • 2+ enabled transports (must choose which one to use)
    ///   • `/reconnect` command (always)
    ///
    /// Selecting an unconfigured transport triggers inline setup before
    /// starting the bridge. `statuses[i]` is parallel to `TRANSPORTS`.
    TransportPick {
        selected: usize,
        ts_available: bool,
        ts_installed: bool,
        /// Pre-computed status badge per transport slot.
        statuses: [String; 3],
    },
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

/// The full wizard state, including current step and mode flags.
pub struct WizardState {
    pub step: WizardStep,
    /// When `true` (triggered by `/reconnect`), skip agent and push setup
    /// steps — only pick transport, then start bridge immediately.
    pub reconnect_mode: bool,
}

impl WizardState {
    /// Compute the first wizard step needed based on current config.
    /// Returns `None` if no wizard is needed (config is complete and exactly
    /// one enabled transport — use it automatically).
    pub fn compute(config: &CommonConfig) -> Option<Self> {
        // 1. Agent command missing?
        if config.agent_command.is_none() {
            return Some(Self { step: WizardStep::AgentSelect { selected: 0 }, reconnect_mode: false });
        }

        // 2. Transport selection:
        //    - 0 enabled → user must pick (and set up) a transport
        //    - 1 enabled → use it automatically (no pick needed)
        //    - 2+ enabled → user must choose which one to run this session
        let enabled_count = config.enabled_transports().len();
        if enabled_count != 1 {
            let ts_available = is_tailscale_available();
            let ts_installed = is_tailscale_installed();
            let statuses = compute_transport_statuses(config, None, ts_available, ts_installed);
            return Some(Self {
                step: WizardStep::TransportPick { selected: 0, ts_available, ts_installed, statuses },
                reconnect_mode: false,
            });
        }

        // 3. Single cloudflare transport present but unconfigured?
        if let Some(cf) = config.transports.get("cloudflare") {
            if cf.enabled && cf.tunnel_id.is_none() {
                return Some(Self {
                    step: WizardStep::CloudflareSetup {
                        fields: [String::new(), String::new(), String::new(), "agent".to_string()],
                        field_idx: 0,
                        error: None,
                    },
                    reconnect_mode: false,
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
                reconnect_mode: false,
            });
        }

        None
    }

    /// Create a wizard in "reconnect" mode that goes straight to transport
    /// selection and starts the bridge immediately after picking.
    pub fn for_reconnect(config: &CommonConfig, active_transport: Option<&str>) -> Self {
        let ts_available = is_tailscale_available();
        let ts_installed = is_tailscale_installed();
        let statuses = compute_transport_statuses(config, active_transport, ts_available, ts_installed);
        Self {
            step: WizardStep::TransportPick { selected: 0, ts_available, ts_installed, statuses },
            reconnect_mode: true,
        }
    }
}

/// Compute status badge strings for all three transport slots.
pub fn compute_transport_statuses(
    config: &CommonConfig,
    active: Option<&str>,
    ts_available: bool,
    ts_installed: bool,
) -> [String; 3] {
    let status_for = |name: &str| -> String {
        let tc = config.transports.get(name);
        let is_enabled = tc.map(|t| t.enabled).unwrap_or(false);
        let is_cf_ready = name != "cloudflare"
            || tc.and_then(|t| t.tunnel_id.as_ref()).is_some();
        let ready = is_enabled && is_cf_ready;

        if ready {
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
                "cloudflare" => "[setup required]".to_string(),
                _ => String::new(),
            }
        }
    };
    [status_for("local"), status_for("tailscale-serve"), status_for("cloudflare")]
}

// ── Input helpers (called from app.rs) ──────────────────────────────────────

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
        WizardStep::TransportPick { selected, .. } => {
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
        WizardStep::TransportPick { selected, .. } => {
            *selected = (*selected + 1).min(TRANSPORTS.len() - 1);
        }
        _ => {}
    }
}

/// Returns the selected agent command when the user confirms agent selection.
/// Returns `None` if they chose "Custom..." (transition to custom input step).
pub fn wizard_confirm_agent(state: &WizardState) -> Option<Option<String>> {
    if let WizardStep::AgentSelect { selected } = &state.step {
        let (_, cmd) = AGENTS[*selected];
        if cmd.is_empty() {
            None // Custom
        } else {
            Some(Some(cmd.to_string()))
        }
    } else {
        None
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

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
            let inner = wizard_panel(frame, title, "↑/↓ navigate   Enter select   unconfigured → inline setup");
            let items: Vec<ListItem> = TRANSPORTS.iter().enumerate().map(|(i, _)| {
                let prefix = if i == *selected { "> " } else { "  " };
                let text = format!("{}{:<30} {}", prefix, TRANSPORT_LABELS[i], statuses[i]);
                ListItem::new(text).style(if i == *selected {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                })
            }).collect();
            let list_area = Rect { y: inner.y + 1, height: inner.height.saturating_sub(2), ..inner };
            frame.render_widget(List::new(items), list_area);
        }

        WizardStep::CloudflareSetup { fields, field_idx, error } => {
            let inner = wizard_panel(
                frame,
                "Cloudflare Zero Trust Setup",
                "Tab next field   Enter submit   Esc back",
            );
            render_form(frame, inner, &[
                "API Token:",
                "Account ID:",
                "Domain (e.g. example.com):",
                "Subdomain [agent]:",
            ], fields, *field_idx, error.as_deref());
        }

        WizardStep::CloudflareLoading => {
            let inner = wizard_panel(frame, "Cloudflare Setup", "");
            let p_area = Rect { y: inner.y + 2, height: 2, ..inner };
            frame.render_widget(
                Paragraph::new("Configuring Cloudflare Zero Trust...")
                    .style(Style::default().fg(Color::Yellow)),
                p_area,
            );
        }

        WizardStep::PushSetup { fields, field_idx, error } => {
            let inner = wizard_panel(
                frame,
                "Push Notifications (optional)",
                "Tab next field   Enter submit   Esc skip",
            );
            render_form(frame, inner, &[
                "Token service URL:",
                "Push service URL:",
                "Client ID:",
                "Client Secret:",
            ], fields, *field_idx, error.as_deref());
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

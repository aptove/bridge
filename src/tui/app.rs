use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use tokio::sync::mpsc;
use tracing::info;

use crate::common_config::{CommonConfig, PushRelayConfig, TransportConfig};
use crate::tui::{
    events::{AppEvent, BridgeEvent},
    screens::{
        popup::{render_popup, PopupKind},
        running::{render_running, RunningState},
        wizard::{
            render_wizard, wizard_backspace, wizard_confirm_agent,
            wizard_move_down, wizard_move_up, wizard_next_field, wizard_type_char, WizardState,
            WizardStep,
        },
    },
    widgets::input_bar::{AcEntry, AutocompleteState},
};
use crate::{cloudflare::CloudflareClient, runner::run_bridge};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// All slash commands with their one-line descriptions.
const COMMANDS: &[(&str, &str)] = &[
    ("/qr",          "Show QR pairing code"),
    ("/status",      "Show configuration status"),
    ("/test-push",   "Send a test push notification"),
    ("/reconnect",   "Restart all transports"),
    ("/config",      "Reconfigure bridge settings"),
    ("/help",        "List commands"),
    ("/quit",        "Exit the bridge"),
];

#[derive(Debug, PartialEq)]
enum Screen {
    Wizard,
    Running,
}

pub struct App {
    screen: Screen,
    wizard: Option<WizardState>,
    popup: Option<PopupKind>,

    // Config (mutable during wizard).
    config: CommonConfig,

    // Running state.
    transport_name: String,
    transport_addr: String,
    transport_up: bool,
    push_up: bool,
    pairing_url: Option<String>,
    qr_string: Option<String>,    // rendered QR (recomputed when pairing_url changes)
    tls_fingerprint: Option<String>,

    // Logs.
    logs: Vec<crate::tui::events::LogRecord>,
    log_scroll: usize,    // 0 = tail; larger = scrolled up
    auto_scroll: bool,

    // Input bar.
    input: String,
    // Autocomplete: indices into COMMANDS that match the current input prefix.
    ac_matches: Vec<usize>,
    ac_idx: usize,

    // Bridge shutdown signal.
    bridge_shutdown: Option<tokio::sync::oneshot::Sender<()>>,

    // Event channel sender (for spawning background tasks).
    event_tx: mpsc::Sender<AppEvent>,

    // Whether quit was requested.
    quit: bool,
}

impl App {
    pub fn new(config: CommonConfig, event_tx: mpsc::Sender<AppEvent>) -> Self {
        let wizard = WizardState::compute(&config);
        let screen = if wizard.is_some() { Screen::Wizard } else { Screen::Running };

        Self {
            screen,
            wizard,
            popup: None,
            config,
            transport_name: String::new(),
            transport_addr: String::new(),
            transport_up: false,
            push_up: false,
            pairing_url: None,
            qr_string: None,
            tls_fingerprint: None,
            logs: Vec::new(),
            log_scroll: 0,
            auto_scroll: true,
            input: String::new(),
            ac_matches: Vec::new(),
            ac_idx: 0,
            bridge_shutdown: None,
            event_tx,
            quit: false,
        }
    }

    /// Start the ratatui terminal, keyboard thread, and run the event loop.
    pub async fn run(mut self, mut event_rx: mpsc::Receiver<AppEvent>) -> Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // If no wizard needed, start bridge immediately.
        if self.screen == Screen::Running {
            self.start_bridge();
        }

        // Main event loop.
        loop {
            terminal.draw(|frame| {
                match self.screen {
                    Screen::Wizard => {
                        if let Some(ref wizard) = self.wizard {
                            render_wizard(frame, wizard);
                        }
                    }
                    Screen::Running => {
                        let running_state = RunningState {
                            transport_name: self.transport_name.clone(),
                            transport_addr: self.transport_addr.clone(),
                            transport_up: self.transport_up,
                            push_up: self.push_up,
                        };
                        // Build autocomplete entries for the renderer (no allocation if empty).
                        let ac_entries: Vec<AcEntry<'_>> = self.ac_matches.iter().map(|&i| AcEntry {
                            command: COMMANDS[i].0,
                            description: COMMANDS[i].1,
                        }).collect();
                        let ac_state = if ac_entries.is_empty() {
                            None
                        } else {
                            Some(AutocompleteState { matches: &ac_entries, selected: self.ac_idx })
                        };
                        render_running(frame, &running_state, &self.logs, self.log_scroll, &self.input, VERSION, ac_state.as_ref());
                        if let Some(ref popup) = self.popup {
                            let status_text = self.build_status_text();
                            render_popup(frame, popup, &self.qr_string, &status_text);
                        }
                    }
                }
            })?;

            if self.quit {
                break;
            }

            match event_rx.recv().await {
                Some(event) => self.handle_event(event).await,
                None => break,
            }
        }

        // Cleanup terminal.
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
        terminal.show_cursor()?;

        // Signal bridge shutdown.
        if let Some(tx) = self.bridge_shutdown.take() {
            let _ = tx.send(());
        }

        Ok(())
    }

    async fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.handle_key(key).await,
            AppEvent::Bridge(ev) => self.handle_bridge_event(ev),
            AppEvent::Log(record) => {
                self.logs.push(record);
                if self.logs.len() > 2000 {
                    self.logs.drain(0..200);
                }
                if self.auto_scroll {
                    self.log_scroll = 0;
                }
            }
            AppEvent::Tick => {}
            AppEvent::Resize(_, _) => {}
            AppEvent::CloudflareSetupResult(result) => {
                self.handle_cloudflare_result(result).await;
            }
            AppEvent::TestPushResult(result) => {
                match result {
                    Ok(true)  => self.log_push("Push notification sent successfully.".to_string()),
                    Ok(false) => self.log_push("No registered devices / debounce active.".to_string()),
                    Err(e)    => self.log_push(format!("Push notification failed: {}", e)),
                }
            }
        }
    }

    async fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        // Ctrl+C always quits.
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }

        match self.screen {
            Screen::Wizard => self.handle_wizard_key(key).await,
            Screen::Running => self.handle_running_key(key).await,
        }
    }

    async fn handle_wizard_key(&mut self, key: crossterm::event::KeyEvent) {
        let Some(ref mut wizard) = self.wizard else { return };

        match key.code {
            KeyCode::Esc => self.handle_wizard_escape().await,
            KeyCode::Up => wizard_move_up(wizard),
            KeyCode::Down => wizard_move_down(wizard),
            KeyCode::Tab => wizard_next_field(wizard),
            KeyCode::Backspace => wizard_backspace(wizard),
            KeyCode::Char(c) => wizard_type_char(wizard, c),
            KeyCode::Enter => self.handle_wizard_enter().await,
            _ => {}
        }
    }

    async fn handle_wizard_escape(&mut self) {
        let Some(ref wizard) = self.wizard else { return };
        match &wizard.step {
            WizardStep::AgentCustomInput { .. } => {
                // Back to agent select.
                if let Some(ref mut w) = self.wizard {
                    w.step = WizardStep::AgentSelect { selected: AGENTS.len() - 1 };
                }
            }
            WizardStep::PushSetup { .. } => {
                // Skip push setup.
                self.advance_past_push();
            }
            _ => {}
        }
    }

    async fn handle_wizard_enter(&mut self) {
        let step = self.wizard.as_ref().map(|w| w.step.clone());
        match step {
            Some(WizardStep::AgentSelect { selected }) => {
                let result = self.wizard.as_ref().and_then(|w| {
                    // Returns None if Custom chosen, Some(cmd) otherwise.
                    match wizard_confirm_agent(w) {
                        Some(maybe_cmd) => maybe_cmd.map(|cmd| (cmd, false)),
                        None => None,
                    }
                });

                if let Some((cmd, _)) = result {
                    self.config.agent_command = Some(cmd);
                    let _ = self.config.save();
                    self.advance_wizard_after_agent().await;
                } else if selected == AGENTS.len() - 1 {
                    // Custom selected.
                    if let Some(ref mut w) = self.wizard {
                        w.step = WizardStep::AgentCustomInput { input: String::new() };
                    }
                }
            }

            Some(WizardStep::AgentCustomInput { ref input }) => {
                if !input.is_empty() {
                    let cmd = input.clone();
                    self.config.agent_command = Some(cmd);
                    let _ = self.config.save();
                    self.advance_wizard_after_agent().await;
                }
            }

            Some(WizardStep::TransportSelect { selected, .. }) => {
                let transport_name = ["local", "tailscale-serve", "cloudflare"][selected];
                match transport_name {
                    "local" => {
                        let port = 8765u16;
                        let tc = TransportConfig {
                            enabled: true,
                            port: Some(port),
                            tls: Some(true),
                            ..Default::default()
                        };
                        self.config.transports.insert("local".to_string(), tc);
                        let _ = self.config.save();
                        self.advance_wizard_after_transport().await;
                    }
                    "tailscale-serve" => {
                        let tc = TransportConfig {
                            enabled: true,
                            port: Some(8766),
                            tls: None,
                            ..Default::default()
                        };
                        self.config.transports.insert("tailscale-serve".to_string(), tc);
                        let _ = self.config.save();
                        self.advance_wizard_after_transport().await;
                    }
                    "cloudflare" => {
                        // Advance to Cloudflare setup form.
                        if let Some(ref mut w) = self.wizard {
                            w.step = WizardStep::CloudflareSetup {
                                fields: [
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    "agent".to_string(),
                                ],
                                field_idx: 0,
                                error: None,
                            };
                        }
                    }
                    _ => {}
                }
            }

            Some(WizardStep::CloudflareSetup { ref fields, field_idx, .. }) => {
                if field_idx < 3 {
                    // Not on last field — advance to next.
                    if let Some(ref mut w) = self.wizard {
                        if let WizardStep::CloudflareSetup { ref mut field_idx, .. } = w.step {
                            *field_idx += 1;
                        }
                    }
                } else {
                    // Last field — submit.
                    let api_token   = fields[0].clone();
                    let account_id  = fields[1].clone();
                    let domain      = fields[2].clone();
                    let subdomain   = if fields[3].is_empty() { "agent".to_string() } else { fields[3].clone() };

                    if api_token.is_empty() || account_id.is_empty() || domain.is_empty() {
                        if let Some(ref mut w) = self.wizard {
                            if let WizardStep::CloudflareSetup { ref mut error, .. } = w.step {
                                *error = Some("All fields except subdomain are required.".to_string());
                            }
                        }
                        return;
                    }

                    // Kick off async Cloudflare setup.
                    if let Some(ref mut w) = self.wizard {
                        w.step = WizardStep::CloudflareLoading;
                    }

                    let event_tx = self.event_tx.clone();
                    tokio::spawn(async move {
                        let result = run_cloudflare_setup(api_token, account_id, domain, subdomain).await
                            .map_err(|e| e.to_string());
                        let _ = event_tx.send(AppEvent::CloudflareSetupResult(result)).await;
                    });
                }
            }

            Some(WizardStep::PushSetup { ref fields, field_idx, .. }) => {
                if field_idx < 3 {
                    if let Some(ref mut w) = self.wizard {
                        if let WizardStep::PushSetup { ref mut field_idx, .. } = w.step {
                            *field_idx += 1;
                        }
                    }
                } else {
                    // Submit push config.
                    let token_url     = fields[0].clone();
                    let push_url      = fields[1].clone();
                    let client_id     = fields[2].clone();
                    let client_secret = fields[3].clone();

                    if client_id.is_empty() || client_secret.is_empty() {
                        if let Some(ref mut w) = self.wizard {
                            if let WizardStep::PushSetup { ref mut error, .. } = w.step {
                                *error = Some("Client ID and secret are required.".to_string());
                            }
                        }
                        return;
                    }

                    self.config.push_relay = Some(PushRelayConfig {
                        url: push_url,
                        token_url,
                        client_id,
                        client_secret,
                    });
                    let _ = self.config.save();
                    self.advance_past_push();
                }
            }

            _ => {}
        }
    }

    async fn advance_wizard_after_agent(&mut self) {
        // Check if transport is already configured.
        if self.config.enabled_transports().is_empty() {
            let ts_available = crate::tailscale::is_tailscale_available();
            let ts_installed = crate::tailscale::is_tailscale_installed();
            if let Some(ref mut w) = self.wizard {
                w.step = WizardStep::TransportSelect { selected: 0, ts_available, ts_installed };
            }
        } else {
            self.advance_wizard_after_transport().await;
        }
    }

    async fn advance_wizard_after_transport(&mut self) {
        // Check if push needs setup.
        if self.config.push_relay.is_none() {
            if let Some(ref mut w) = self.wizard {
                w.step = WizardStep::PushSetup {
                    fields: [
                        "https://token.aptove.com".to_string(),
                        "https://push.aptove.com".to_string(),
                        String::new(),
                        String::new(),
                    ],
                    field_idx: 0,
                    error: None,
                };
            }
        } else {
            self.finish_wizard();
        }
    }

    fn advance_past_push(&mut self) {
        self.finish_wizard();
    }

    fn finish_wizard(&mut self) {
        if let Some(ref mut w) = self.wizard {
            w.step = WizardStep::Done;
        }
        self.screen = Screen::Running;
        self.wizard = None;
        self.start_bridge();
    }

    async fn handle_cloudflare_result(&mut self, result: Result<TransportConfig, String>) {
        match result {
            Ok(tc) => {
                self.config.transports.insert("cloudflare".to_string(), tc);
                let _ = self.config.save();
                self.advance_wizard_after_transport().await;
            }
            Err(e) => {
                // Revert to CF form with error.
                if let Some(ref mut w) = self.wizard {
                    w.step = WizardStep::CloudflareSetup {
                        fields: [String::new(), String::new(), String::new(), "agent".to_string()],
                        field_idx: 0,
                        error: Some(e),
                    };
                }
            }
        }
    }

    fn start_bridge(&mut self) {
        let config = self.config.clone();
        let event_tx = self.event_tx.clone();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.bridge_shutdown = Some(shutdown_tx);

        tokio::spawn(async move {
            if let Err(e) = run_bridge(config, event_tx.clone(), shutdown_rx).await {
                let _ = event_tx.send(AppEvent::Bridge(BridgeEvent::BridgeError {
                    message: e.to_string(),
                })).await;
            }
        });
    }

    async fn handle_running_key(&mut self, key: crossterm::event::KeyEvent) {
        // Popup dismissal.
        if self.popup.is_some() {
            if key.code == KeyCode::Esc || key.code == KeyCode::Enter {
                self.popup = None;
            }
            return;
        }

        let ac_open = !self.ac_matches.is_empty();

        match key.code {
            KeyCode::Esc => {
                self.input.clear();
                self.update_autocomplete();
            }

            // Up/Down: navigate autocomplete when open, else scroll log.
            KeyCode::Up if ac_open => {
                self.ac_idx = self.ac_idx.saturating_sub(1);
            }
            KeyCode::Down if ac_open => {
                self.ac_idx = (self.ac_idx + 1).min(self.ac_matches.len().saturating_sub(1));
            }

            // Tab: accept the selected suggestion (complete input, don't submit).
            KeyCode::Tab if ac_open => {
                let cmd = COMMANDS[self.ac_matches[self.ac_idx]].0.to_string();
                self.input = cmd;
                self.update_autocomplete();
            }

            // Enter: accept selected suggestion + submit; or submit raw input.
            KeyCode::Enter => {
                let cmd = if ac_open {
                    COMMANDS[self.ac_matches[self.ac_idx]].0.to_string()
                } else {
                    self.input.trim().to_string()
                };
                self.input.clear();
                self.update_autocomplete();
                if !cmd.is_empty() {
                    self.handle_command(&cmd).await;
                }
            }

            KeyCode::Backspace => {
                self.input.pop();
                self.update_autocomplete();
            }

            KeyCode::Char(c) => {
                self.input.push(c);
                self.update_autocomplete();
            }

            KeyCode::PageUp => {
                self.log_scroll = self.log_scroll.saturating_add(10);
                self.auto_scroll = false;
            }
            KeyCode::PageDown => {
                if self.log_scroll > 10 {
                    self.log_scroll -= 10;
                } else {
                    self.log_scroll = 0;
                    self.auto_scroll = true;
                }
            }
            _ => {}
        }
    }

    /// Recompute `ac_matches` from the current `input`.
    ///
    /// Shows all commands when the user has only typed `/`, then narrows
    /// as they type more. Clears when input is empty or lacks a `/` prefix.
    fn update_autocomplete(&mut self) {
        self.ac_matches.clear();
        self.ac_idx = 0;

        if !self.input.starts_with('/') {
            return;
        }

        let prefix = self.input.as_str();
        for (i, &(cmd, _)) in COMMANDS.iter().enumerate() {
            if cmd.starts_with(prefix) {
                self.ac_matches.push(i);
            }
        }

        // If only one exact match remains and input IS the command, clear
        // the list so it doesn't obscure the screen on a completed word.
        if self.ac_matches.len() == 1 && COMMANDS[self.ac_matches[0]].0 == prefix {
            self.ac_matches.clear();
        }
    }

    async fn handle_command(&mut self, cmd: &str) {
        match cmd {
            "/qr" => {
                self.popup = Some(PopupKind::QrCode);
            }
            "/status" => {
                self.popup = Some(PopupKind::Status);
            }
            "/help" => {
                self.popup = Some(PopupKind::Help);
            }
            "/quit" | "/exit" => {
                self.quit = true;
            }
            "/reconnect" => {
                self.log_push("Reconnect not yet implemented — restart the bridge.".to_string());
            }
            "/test-push" => {
                self.handle_test_push();
            }
            "/config" => {
                // Re-run wizard from the beginning.
                self.wizard = Some(WizardState {
                    step: crate::tui::screens::wizard::WizardStep::AgentSelect { selected: 0 },
                });
                self.screen = Screen::Wizard;
            }
            other => {
                self.log_push(format!("Unknown command: {}  (type /help for list)", other));
            }
        }
    }

    fn handle_test_push(&mut self) {
        let push_cfg = match &self.config.push_relay {
            Some(p) if !p.url.is_empty() && !p.client_id.is_empty() => p.clone(),
            _ => {
                self.log_push("Push relay not configured.".to_string());
                return;
            }
        };
        let event_tx = self.event_tx.clone();
        tokio::spawn(async move {
            use crate::push::PushRelayClient;
            let client = PushRelayClient::new(push_cfg.url.clone(), String::new())
                .with_jwt_credentials(push_cfg.token_url.clone(), push_cfg.client_id.clone(), push_cfg.client_secret.clone());
            let result = client.notify("test").await.map_err(|e| e.to_string());
            let _ = event_tx.send(AppEvent::TestPushResult(result)).await;
        });
        self.log_push("Sending test push notification...".to_string());
    }

    fn handle_bridge_event(&mut self, event: BridgeEvent) {
        match event {
            BridgeEvent::TransportUp { name, addr } => {
                self.transport_name = name.clone();
                self.transport_addr = addr.clone();
                self.transport_up = true;
                self.log_push(format!("Transport up: {} — {}", name, addr));
            }
            BridgeEvent::TransportDown { name } => {
                self.transport_up = false;
                self.log_push(format!("Transport down: {}", name));
            }
            BridgeEvent::ClientConnected { session_id } => {
                self.log_push(format!("Client connected (session {})", session_id));
            }
            BridgeEvent::ClientDisconnected { session_id } => {
                self.log_push(format!("Client disconnected (session {})", session_id));
            }
            BridgeEvent::PairingCompleted => {
                self.log_push("Pairing completed.".to_string());
            }
            BridgeEvent::PairingUrlReady { url, transport } => {
                info!("Pairing URL ready for transport: {}", transport);
                self.pairing_url = Some(url.clone());
                // Pre-render QR string.
                if let Ok(qr) = crate::qr::render_qr_code(&url) {
                    self.qr_string = Some(qr);
                }
            }
            BridgeEvent::AgentSpawned { command } => {
                self.log_push(format!("Agent spawned: {}", command));
            }
            BridgeEvent::AgentExited => {
                self.log_push("Agent process exited.".to_string());
            }
            BridgeEvent::TlsFingerprint { fingerprint } => {
                self.tls_fingerprint = Some(fingerprint.clone());
                self.log_push(format!("TLS fingerprint: {}", fingerprint));
            }
            BridgeEvent::PushRegistered => {
                self.push_up = true;
                self.log_push("Push token registered.".to_string());
            }
            BridgeEvent::BridgeStopped => {
                self.transport_up = false;
                self.log_push("Bridge stopped.".to_string());
            }
            BridgeEvent::BridgeError { message } => {
                self.log_push(format!("Bridge error: {}", message));
            }
        }
    }

    fn log_push(&mut self, msg: String) {
        use crate::tui::events::LogRecord;
        let now = chrono::Local::now();
        self.logs.push(LogRecord {
            timestamp: now.format("%H:%M:%S").to_string(),
            level: "INFO ".to_string(),
            message: msg,
        });
        if self.auto_scroll {
            self.log_scroll = 0;
        }
    }

    fn build_status_text(&self) -> String {
        let mut lines = vec![
            format!("Agent ID:    {}", if self.config.agent_id.is_empty() { "(not set)" } else { &self.config.agent_id }),
            format!("Agent cmd:   {}", self.config.agent_command.as_deref().unwrap_or("(not set)")),
            format!("Config dir:  {}", CommonConfig::config_dir().display()),
            String::new(),
            "Transports:".to_string(),
        ];
        if self.config.transports.is_empty() {
            lines.push("  (none)".to_string());
        } else {
            let mut names: Vec<_> = self.config.transports.keys().collect();
            names.sort();
            for name in names {
                let t = &self.config.transports[name];
                let status = if t.enabled { "enabled" } else { "disabled" };
                let port_str = t.port.map(|p| format!(":{}", p)).unwrap_or_default();
                lines.push(format!("  {:<22} {} {}", name, status, port_str));
            }
        }
        if let Some(ref tls) = self.tls_fingerprint {
            lines.push(String::new());
            lines.push(format!("TLS fingerprint: {}", tls));
        }
        if let Some(ref pr) = self.config.push_relay {
            lines.push(String::new());
            lines.push(format!("Push relay: {}", pr.url));
        }
        lines.join("\n")
    }
}

// ── Background async helpers ─────────────────────────────────────────────────

/// Run the Cloudflare Zero Trust setup API calls.
async fn run_cloudflare_setup(
    api_token: String,
    account_id: String,
    domain: String,
    subdomain: String,
) -> anyhow::Result<TransportConfig> {
    use crate::cloudflare::{write_credentials_file, write_cloudflared_config_at};

    let client = CloudflareClient::new(api_token, account_id.clone());
    let hostname = format!("{}.{}", subdomain, domain);
    let tunnel_name = format!("{}-tunnel", domain.split('.').next().unwrap_or("bridge"));

    info!("Creating Cloudflare tunnel: {}", tunnel_name);
    let tunnel = client.create_or_get_tunnel(&tunnel_name).await?;

    info!("Creating DNS record for {}", hostname);
    client.create_dns_record(&domain, &subdomain, &tunnel.id).await?;

    info!("Creating Access Application...");
    let _ = client.create_access_application(&hostname).await?;

    info!("Generating Service Token...");
    let service_token = client.create_service_token(&hostname).await?;

    info!("Configuring tunnel ingress...");
    client.configure_tunnel_ingress(&tunnel.id, &hostname, 8080).await?;

    let credentials_path = write_credentials_file(&account_id, &tunnel.id, &tunnel.secret)?;
    let config_dir = crate::common_config::CommonConfig::config_dir();
    let per_project_config = config_dir.join("cloudflared.yml");
    write_cloudflared_config_at(&tunnel.id, &credentials_path, &hostname, 8080, &per_project_config)?;

    info!("Cloudflare setup complete for {}", hostname);

    Ok(TransportConfig {
        enabled: true,
        port: Some(8080),
        tls: None,
        hostname: Some(format!("https://{}", hostname)),
        tunnel_id: Some(tunnel.id),
        tunnel_secret: Some(tunnel.secret),
        account_id: Some(account_id),
        client_id: Some(service_token.client_id),
        client_secret: Some(service_token.client_secret),
        domain: Some(domain),
        subdomain: Some(subdomain),
    })
}

use crate::tui::screens::wizard::AGENTS;

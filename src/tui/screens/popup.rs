use ratatui::Frame;

use crate::tui::widgets::qr_popup::{render_qr_popup, render_text_popup};

#[derive(Debug, Clone, PartialEq)]
pub enum PopupKind {
    QrCode,
    Status,
    Help,
}

const HELP_TEXT: &str = "\
/qr          Show QR pairing code
/status      Show configuration status
/test-push   Send a test push notification
/reconnect   Restart all transports
/config      Reconfigure bridge settings
/help        Show this help
/quit        Exit the bridge
";

pub fn render_popup(
    frame: &mut Frame,
    kind: &PopupKind,
    qr_string: &Option<String>,
    status_text: &str,
) {
    match kind {
        PopupKind::QrCode => {
            let qr = qr_string.as_deref().unwrap_or("No QR code available yet.\nStart the bridge first.");
            render_qr_popup(frame, frame.area(), "Pairing QR Code (Esc to close)", qr);
        }
        PopupKind::Status => {
            render_text_popup(frame, frame.area(), "Bridge Status (Esc to close)", status_text);
        }
        PopupKind::Help => {
            render_text_popup(frame, frame.area(), "Commands (Esc to close)", HELP_TEXT);
        }
    }
}

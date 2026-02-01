use anyhow::{Context, Result};
use qrcode::{QrCode, EcLevel};
use crate::config::BridgeConfig;

/// Unicode block characters for compact QR rendering
/// Uses upper/lower half blocks to fit 2 rows per line
const BOTH_BLACK: &str = "█";
const TOP_BLACK: &str = "▀";
const BOTTOM_BLACK: &str = "▄";
const BOTH_WHITE: &str = " ";

/// Display a QR code in the terminal for mobile scanning
pub fn display_qr_code(config: &BridgeConfig) -> Result<()> {
    let connection_json = config.to_connection_json()?;
    
    // Use lower error correction to reduce QR code size
    let code = QrCode::with_error_correction_level(connection_json.as_bytes(), EcLevel::L)
        .context("Failed to generate QR code")?;
    
    let modules = code.to_colors();
    let width = code.width();
    
    // Render using Unicode half-blocks for compact display
    // Each character represents 2 vertical modules
    let mut output = String::new();
    
    // Add quiet zone (1 row of white)
    output.push_str("\n");
    for _ in 0..width + 4 {
        output.push(' ');
    }
    output.push('\n');
    
    // Process 2 rows at a time using half-block characters
    for row in (0..width).step_by(2) {
        // Quiet zone left
        output.push_str("  ");
        
        for col in 0..width {
            let top_idx = row * width + col;
            let bottom_idx = (row + 1) * width + col;
            
            let top_dark = modules[top_idx] == qrcode::Color::Dark;
            let bottom_dark = if row + 1 < width {
                modules[bottom_idx] == qrcode::Color::Dark
            } else {
                false // Treat out-of-bounds as white
            };
            
            let block = match (top_dark, bottom_dark) {
                (true, true) => BOTH_BLACK,
                (true, false) => TOP_BLACK,
                (false, true) => BOTTOM_BLACK,
                (false, false) => BOTH_WHITE,
            };
            output.push_str(block);
        }
        
        // Quiet zone right
        output.push_str("  ");
        output.push('\n');
    }
    
    // Add quiet zone (1 row of white)
    for _ in 0..width + 4 {
        output.push(' ');
    }
    output.push('\n');
    
    println!("{}", output);
    println!("Connection Details:");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("URL: {}", config.hostname);
    if config.client_id.is_empty() {
        println!("Client ID: N/A (direct websocket)");
    } else {
        println!("Client ID: {}...", &config.client_id[..20.min(config.client_id.len())]);
    }
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    
    Ok(())
}

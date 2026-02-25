use anyhow::{Context, Result};
use qrcode::{QrCode, EcLevel};
use crate::pairing::PairingManager;
use std::path::PathBuf;

/// Unicode block characters for compact QR rendering
/// Uses upper/lower half blocks to fit 2 rows per line
const BOTH_BLACK: &str = "█";
const TOP_BLACK: &str = "▀";
const BOTTOM_BLACK: &str = "▄";
const BOTH_WHITE: &str = " ";

/// Save a QR code as a PNG image file for easier scanning
fn save_qr_code_image(data: &str, path: &PathBuf) -> Result<()> {
    use image::{Luma, GrayImage};
    
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::L)
        .context("Failed to generate QR code")?;
    
    let width = code.width();
    let scale = 10; // 10 pixels per module
    let border = 4;  // 4 module quiet zone
    let img_size = (width + border * 2) * scale;
    
    let mut img = GrayImage::from_pixel(img_size as u32, img_size as u32, Luma([255u8]));
    
    for (y, row) in code.to_colors().chunks(width).enumerate() {
        for (x, &color) in row.iter().enumerate() {
            if color == qrcode::Color::Dark {
                // Draw a scaled black square
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = ((x + border) * scale + dx) as u32;
                        let py = ((y + border) * scale + dy) as u32;
                        img.put_pixel(px, py, Luma([0u8]));
                    }
                }
            }
        }
    }
    
    img.save(path).context("Failed to save QR code image")?;
    Ok(())
}

/// Render a QR code to a string for terminal display
fn render_qr_code(data: &str) -> Result<String> {
    // Use lower error correction to reduce QR code size
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::L)
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
    
    Ok(output)
}

/// Display a QR code with pairing URL for secure mobile connection.
///
/// `hostname` is the WebSocket URL (e.g. `wss://192.168.1.1:8765`); it is
/// converted to HTTPS/HTTP for the pairing endpoint.
pub fn display_qr_code_with_pairing(hostname: &str, pairing: &PairingManager) -> Result<()> {
    // Build the base URL for pairing (HTTPS)
    let base_url = hostname.replace("wss://", "https://").replace("ws://", "http://");
    let pairing_url = pairing.get_pairing_url(&base_url);
    
    // Render the QR code
    let qr_output = render_qr_code(&pairing_url)?;
    
    // Save QR code as image for easier scanning
    let qr_image_path = std::env::temp_dir().join("bridge_pairing_qr.png");
    if let Err(e) = save_qr_code_image(&pairing_url, &qr_image_path) {
        tracing::warn!("Could not save QR code image: {}", e);
    }
    
    // Display expiration notice
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  ⏱️  QR code expires in {} seconds | Single use only", pairing.seconds_remaining());
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    // Display QR code
    println!("{}", qr_output);
    
    // Display the full pairing URL and image path
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  📱 Scan QR code with your mobile app");
    println!("  🔗 {}", pairing_url);
    if qr_image_path.exists() {
        println!("  🖼️  QR image saved to: {}", qr_image_path.display());
        println!("     (Open this file if terminal QR code doesn't scan)");
    }
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    
    Ok(())
}

/// Display a static QR code in the terminal for mobile scanning (no pairing handshake).
///
/// `connection_json` is the pre-built JSON string to encode (e.g. from
/// `CommonConfig::to_connection_json()` or `BridgeConfig::to_connection_json()`).
pub fn display_qr_code(connection_json: &str, transport: &str) -> Result<()> {
    // Render the QR code
    let qr_output = render_qr_code(connection_json)?;

    println!("{}", qr_output);

    // Parse and pretty-print the QR code content
    let json_value: serde_json::Value = serde_json::from_str(connection_json)
        .context("Failed to parse connection JSON")?;
    
    println!("QR Code Content:");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    // Print each field with appropriate masking for sensitive data
    if let Some(agent_id) = json_value.get("agentId").and_then(|v| v.as_str()) {
        println!("  Agent ID:        {}", agent_id);
    }
    if let Some(url) = json_value.get("url").and_then(|v| v.as_str()) {
        println!("  URL:             {}", url);
    }
    if let Some(protocol) = json_value.get("protocol").and_then(|v| v.as_str()) {
        println!("  Protocol:        {}", protocol);
    }
    if let Some(version) = json_value.get("version").and_then(|v| v.as_str()) {
        println!("  Version:         {}", version);
    }
    if let Some(client_id) = json_value.get("clientId").and_then(|v| v.as_str()) {
        if client_id.len() > 8 {
            println!("  Client ID:       {}...{}", &client_id[..4], &client_id[client_id.len()-4..]);
        } else {
            println!("  Client ID:       {}", client_id);
        }
    }
    if let Some(client_secret) = json_value.get("clientSecret").and_then(|v| v.as_str()) {
        println!("  Client Secret:   {}... (hidden)", &client_secret[..4.min(client_secret.len())]);
    }
    if let Some(auth_token) = json_value.get("authToken").and_then(|v| v.as_str()) {
        println!("  Auth Token:      {}... (hidden)", &auth_token[..4.min(auth_token.len())]);
    }
    if let Some(fingerprint) = json_value.get("certFingerprint").and_then(|v| v.as_str()) {
        if fingerprint.len() > 16 {
            println!("  TLS Fingerprint: {}...{}", &fingerprint[..8], &fingerprint[fingerprint.len()-8..]);
        } else {
            println!("  TLS Fingerprint: {}", fingerprint);
        }
    }
    
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    let mode_label = match transport {
        "cloudflare"      => "Cloudflare Zero Trust (internet accessible)",
        "tailscale-serve" => "Tailscale (HTTPS via MagicDNS)",
        "tailscale-ip"    => "Tailscale (direct IP)",
        _                 => "Local Network",
    };
    println!("  Mode: {}", mode_label);
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    
    Ok(())
}

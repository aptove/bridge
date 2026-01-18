use anyhow::{Context, Result};
use qrcode::QrCode;
use crate::config::BridgeConfig;

/// Display a QR code in the terminal for mobile scanning
pub fn display_qr_code(config: &BridgeConfig) -> Result<()> {
    let connection_json = config.to_connection_json()?;
    
    let code = QrCode::new(connection_json.as_bytes())
        .context("Failed to generate QR code")?;
    
    let string = code.render::<char>()
        .quiet_zone(false)
        .module_dimensions(2, 1)
        .build();
    
    println!("\n{}", string);
    println!("\nConnection Details:");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("URL: {}", config.hostname);
    println!("Client ID: {}...", &config.client_id[..20.min(config.client_id.len())]);
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    
    Ok(())
}

# Quick Reference Guide

## Prerequisites

### 1. Cloudflare Setup
- **Domain**: Must be using Cloudflare nameservers
- **Zero Trust**: Enable at dash.cloudflare.com (requires payment method, free tier OK)
- **API Token**: Create with these permissions:
  - Cloudflare One → Connectors → Edit
  - Access → Apps and Policies → Edit  
  - Access → Service Tokens → Edit
  - DNS → Zone → Edit

### 2. Get Your Account ID
- Found in Cloudflare Dashboard URL
- Or in the right sidebar of your domain overview

## Installation

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build the bridge
cd /path/to/bridge
cargo build --release

# Binary location: target/release/bridge
```

## Usage

### First Time Setup

```bash
export CLOUDFLARE_API_TOKEN="your_api_token_here"
export CLOUDFLARE_ACCOUNT_ID="your_account_id_here"

./target/release/bridge setup \
  --domain "yourdomain.com" \
  --subdomain "agent"
```

**Output**: QR code + config saved to `~/.config/bridge/config.json`

### Start the Bridge

```bash
# For Gemini CLI
./target/release/bridge start \
  --agent-command "gemini --experimental-acp" \
  --qr

# For Goose
./target/release/bridge start \
  --agent-command "goose" \
  --qr
```

### Show QR Code Again

```bash
./target/release/bridge show-qr
```

### Check Status

```bash
./target/release/bridge status
```

## Mobile App Integration

### 1. Scan QR Code
The QR contains JSON:
```json
{
  "url": "https://agent.yourdomain.com",
  "clientId": "xxxxx.access",
  "clientSecret": "xxxxxxxxxxxxxx",
  "protocol": "acp",
  "version": "1.0"
}
```

### 2. Store in Keychain
Never store in plain text or UserDefaults!

### 3. Connect with Headers
```swift
var request = URLRequest(url: URL(string: "wss://agent.yourdomain.com")!)
request.addValue(clientID, forHTTPHeaderField: "CF-Access-Client-Id")
request.addValue(clientSecret, forHTTPHeaderField: "CF-Access-Client-Secret")

let ws = URLSession.shared.webSocketTask(with: request)
ws.resume()
```

### 4. Send ACP Initialize
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "capabilities": {},
    "clientInfo": {
      "name": "iOS Client",
      "version": "1.0.0"
    }
  }
}
```

## Common Issues

### "Zone not found"
- Check domain uses Cloudflare nameservers: `dig NS yourdomain.com`

### "Failed to create tunnel"
- Verify Zero Trust is enabled
- Check API token permissions
- Ensure payment method is added (even for free tier)

### "403 Forbidden" from mobile
- Verify headers are included: `CF-Access-Client-Id` and `CF-Access-Client-Secret`
- Check token hasn't been revoked in dashboard

### Agent fails to start
- Test command manually: `gemini --experimental-acp`
- Check agent is installed and in PATH

## File Locations

- **Config**: `~/.config/bridge/config.json`
- **Binary**: `target/release/bridge`
- **Logs**: stdout (use `tee` to save)

## Security Notes

1. **Protect config.json**: Contains Service Token secret
   ```bash
   chmod 600 ~/.config/bridge/config.json
   ```

2. **QR codes**: Treat as sensitive - they contain credentials

3. **Token rotation**: Re-run `setup` to generate new tokens

4. **Revoke tokens**: Cloudflare Dashboard → Zero Trust → Settings → Service Authentication

## Architecture Flow

```
iPhone App (Swift)
    ↓ WebSocket (wss://)
    ↓ + CF-Access headers
Cloudflare Zero Trust
    ↓ Validates Service Token
    ↓ Routes to tunnel
Cloudflare Tunnel (cloudflared)
    ↓ Local connection
Bridge (Rust - port 8080)
    ↓ stdio pipe
ACP Agent (Gemini/Goose)
```

## Free Tier Limits

| Feature | Limit | Impact |
|---------|-------|--------|
| Users | 50 | Plenty for personal use |
| Tunnels | ∞ | No limit |
| Subdomain levels | 1 | Use `agent.domain.com` not `x.agent.domain.com` |
| Logs | 24h | Enough for debugging |
| SSL | Universal | Single-level subdomains covered |

## Commands Summary

| Command | Purpose |
|---------|---------|
| `setup` | Create Cloudflare infrastructure |
| `start` | Run the bridge server |
| `show-qr` | Display connection QR |
| `status` | Check configuration |

## Environment Variables

| Variable | Purpose | Required |
|----------|---------|----------|
| `CLOUDFLARE_API_TOKEN` | API authentication | For `setup` |
| `CLOUDFLARE_ACCOUNT_ID` | Account identifier | For `setup` |

## Example: Complete Flow

```bash
# 1. Setup (once)
export CLOUDFLARE_API_TOKEN="..."
export CLOUDFLARE_ACCOUNT_ID="..."
cargo run --release -- setup --domain "example.com"

# 2. Save the QR code (scan with mobile)

# 3. Start bridge
cargo run --release -- start --agent-command "gemini --experimental-acp"

# 4. Mobile app connects and sends initialize

# 5. Start chatting with your agent!
```

## Next Steps

1. Build: `cargo build --release`
2. Setup: Run `setup` command
3. Scan: Use mobile app to scan QR
4. Start: Run `start` command
5. Connect: Mobile app connects via WebSocket
6. Chat: Send ACP messages to your agent

## Support

- **Issues**: Check IMPLEMENTATION.md for details
- **Examples**: See `examples/` directory
- **Swift Client**: See `examples/swift-client.swift`

---

**Remember**: This entire setup works on Cloudflare's FREE tier! 🎉

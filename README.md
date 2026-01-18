# ACP Cloudflare Bridge

A secure bridge between stdio-based Agent Client Protocol (ACP) agents and mobile applications via Cloudflare Zero Trust.

## Features

- 🔐 **Automated Zero Trust Setup**: Programmatically creates tunnels, DNS records, and Access policies
- 📱 **QR Code Authentication**: Mobile apps scan a QR code to connect - no manual configuration
- 🌐 **Global Access**: Connect to your local AI agents from anywhere via Cloudflare's network
- 🔒 **Service Token Auth**: Uses Cloudflare Access Service Tokens for secure, credential-based authentication
- ⚡ **WebSocket Streaming**: Real-time bidirectional communication between mobile and agent
- 🦀 **Rust Performance**: Low-latency, high-throughput bridge implementation

## Architecture

```
┌─────────────┐        ┌──────────────────┐        ┌─────────────┐        ┌──────────────┐
│   iPhone    │◄──────►│  Cloudflare      │◄──────►│   Bridge    │◄──────►│  ACP Agent   │
│   Swift     │  WSS   │  Zero Trust      │  HTTP  │  (Rust)     │  stdio │  (Gemini)    │
│   App       │        │  Tunnel          │        │  WebSocket  │        │              │
└─────────────┘        └──────────────────┘        └─────────────┘        └──────────────┘
     ▲                                                     │
     │                                                     │
     └─────────────── QR Code Scan ──────────────────────┘
              (hostname + Service Token)
```

## Prerequisites

### Cloudflare Requirements

1. **Free Cloudflare Account** with:
   - A domain added and using Cloudflare nameservers
   - Zero Trust enabled (requires adding a payment method for verification, but free tier is sufficient)

2. **API Token** with these permissions:
   - **Cloudflare One** → Connectors → Edit
   - **Access** → Apps and Policies → Edit
   - **Access** → Service Tokens → Edit
   - **DNS** → Zone → Edit

3. **Account ID**: Found in your Cloudflare dashboard URL or in the domain overview

### Local Requirements

- Rust 1.70+ (install via [rustup](https://rustup.rs/))
- An ACP-compatible agent (e.g., Gemini CLI with `--experimental-acp` flag)

## Installation

```bash
# Clone the repository
git clone https://github.com/yourusername/acp-cloudflare-bridge.git
cd acp-cloudflare-bridge

# Build the tool
cargo build --release

# Install to PATH
cargo install --path .
```

## Quick Start

### Step 1: Setup Cloudflare Infrastructure

Run the setup command with your Cloudflare credentials:

```bash
acp-bridge setup \
  --api-token "your_cloudflare_api_token" \
  --account-id "your_account_id" \
  --domain "yourdomain.com" \
  --subdomain "agent"
```

This will:
- ✅ Create a Cloudflare Tunnel named "mobile-acp-bridge"
- ✅ Create DNS record `agent.yourdomain.com` → tunnel
- ✅ Create Zero Trust Access Application with Service Auth policy
- ✅ Generate Service Token for mobile authentication
- ✅ Save configuration to `~/.config/acp-cloudflare-bridge/config.json`
- ✅ Display a QR code for mobile connection

**Important**: The QR code contains your Service Token secret. Keep it secure!

### Step 2: Start the Bridge

Start the bridge with your ACP agent command:

```bash
# For Gemini CLI
acp-bridge start --agent-command "gemini --experimental-acp" --qr

# For other agents
acp-bridge start --agent-command "your-agent-command" --qr
```

The `--qr` flag displays the connection QR code again.

### Step 3: Connect Your Mobile App

In your Swift app, scan the QR code. The bridge will provide a JSON payload:

```json
{
  "url": "https://agent.yourdomain.com",
  "clientId": "xxxxx.access",
  "clientSecret": "xxxxxxxxxxxxxx",
  "protocol": "acp",
  "version": "1.0"
}
```

Your Swift app should:

1. **Parse the QR code** and extract credentials
2. **Store in Keychain** for persistent authentication
3. **Connect via WebSocket** with Cloudflare Access headers:

```swift
var request = URLRequest(url: URL(string: "wss://agent.yourdomain.com")!)
request.addValue(clientID, forHTTPHeaderField: "CF-Access-Client-Id")
request.addValue(clientSecret, forHTTPHeaderField: "CF-Access-Client-Secret")

let webSocketTask = URLSession.shared.webSocketTask(with: request)
webSocketTask.resume()
```

## Commands

### `setup`

Create or update Cloudflare Zero Trust infrastructure:

```bash
acp-bridge setup \
  --api-token <TOKEN> \
  --account-id <ID> \
  --domain <DOMAIN> \
  --subdomain <SUBDOMAIN> \
  --tunnel-name <NAME>
```

**Options:**
- `--api-token`: Cloudflare API token (or set `CLOUDFLARE_API_TOKEN` env var)
- `--account-id`: Cloudflare account ID (or set `CLOUDFLARE_ACCOUNT_ID` env var)
- `--domain`: Your domain managed by Cloudflare
- `--subdomain`: Subdomain for the bridge (default: `agent`)
- `--tunnel-name`: Tunnel name (default: `mobile-acp-bridge`)

### `start`

Start the WebSocket bridge server:

```bash
acp-bridge start \
  --agent-command <COMMAND> \
  --port <PORT> \
  --qr
```

**Options:**
- `--agent-command`: Command to spawn the ACP agent (e.g., `"gemini --experimental-acp"`)
- `--port`: Local WebSocket port (default: `8080`)
- `--qr`: Display QR code on startup

### `show-qr`

Display the connection QR code:

```bash
acp-bridge show-qr
```

### `status`

Check configuration status:

```bash
acp-bridge status
```

## Security Considerations

### Service Token Lifecycle

- **Generation**: Service Tokens are generated during `setup` and stored locally
- **Duration**: Tokens are valid for 1 year by default
- **Rotation**: To rotate tokens, run `setup` again (old tokens remain valid)
- **Revocation**: Delete tokens via the Cloudflare Dashboard → Zero Trust → Settings → Service Authentication

### Best Practices

1. **Secure Storage**: The config file contains sensitive credentials. Use appropriate file permissions:
   ```bash
   chmod 600 ~/.config/acp-cloudflare-bridge/config.json
   ```

2. **One-Time QR**: For production, consider implementing QR expiration (show once, then require re-authentication)

3. **Network Isolation**: The bridge binds to `0.0.0.0` by default. For additional security, bind to `127.0.0.1` if your tunnel runs on the same machine

4. **Mobile Keychain**: Always store Service Tokens in the iOS/Android Keychain, never in UserDefaults or plain files

## Limitations on Free Tier

| Feature | Free Tier Limit | Notes |
|---------|-----------------|-------|
| Users | 50 seats | More than enough for personal use |
| Tunnels | Unlimited | Named tunnels only |
| Subdomain Levels | 1 level | Use `agent.domain.com`, not `my.agent.domain.com` |
| Log Retention | 24 hours | Sufficient for debugging |
| SSL Certificate | Universal SSL | Covers single-level subdomains |

## Troubleshooting

### "Zone not found"

Ensure your domain is added to Cloudflare and uses Cloudflare nameservers. Check with:

```bash
dig NS yourdomain.com
```

You should see Cloudflare nameservers (e.g., `*.ns.cloudflare.com`).

### "Failed to create tunnel"

Verify:
1. Zero Trust is enabled (Cloudflare Dashboard → Zero Trust)
2. Payment method is added (required for verification, even on free tier)
3. API token has correct permissions

### Mobile app receives 403 Forbidden

Check:
1. Service Token headers are included in WebSocket request
2. Access Application policy includes "Service Auth"
3. Token hasn't been revoked in Cloudflare Dashboard

### Agent process fails to start

Test your agent command manually:
```bash
gemini --experimental-acp
```

Ensure it accepts stdin and produces stdout (JSON-RPC format).

## Development

### Project Structure

```
src/
├── main.rs           # CLI entry point and command routing
├── cloudflare.rs     # Cloudflare API client
├── bridge.rs         # WebSocket ↔ stdio bridge
├── config.rs         # Configuration management
└── qr.rs            # QR code generation
```

### Running Tests

```bash
cargo test
```

### Building for Release

```bash
cargo build --release
# Binary: target/release/acp-bridge
```

## Contributing

Contributions are welcome! Please:

1. Fork the repository
2. Create a feature branch
3. Add tests for new functionality
4. Submit a pull request

## License

MIT License - see [LICENSE](LICENSE) for details

## Acknowledgments

- Built for the [Agent Client Protocol (ACP)](https://github.com/google/acp) ecosystem
- Inspired by the Language Server Protocol (LSP) design
- Uses Cloudflare's excellent Zero Trust platform

## Related Projects

- [Gemini CLI](https://github.com/google/gemini-cli) - Official ACP reference implementation
- [Goose](https://github.com/block/goose) - Another ACP-compatible agent
- Your Swift ACP client library (coming soon!)

---

**Questions or Issues?** Open an issue on GitHub or contact [@yourusername](https://github.com/yourusername)

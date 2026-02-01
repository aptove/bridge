# ACP Bridge

A local bridge between stdio-based Agent Client Protocol (ACP) agents and mobile applications via WebSocket.

> **Note:** This bridge also includes experimental Cloudflare Zero Trust integration for remote access. The Cloudflare implementation is experimental and may have issues. For reliable usage, we recommend the local WebSocket mode documented below.

## Features

- 📱 **QR Code Connection**: Mobile apps scan a QR code to connect to your local agent
- ⚡ **WebSocket Streaming**: Real-time bidirectional communication between mobile and agent
- 🦀 **Rust Performance**: Low-latency, high-throughput bridge implementation
- 🔌 **STDIO Proxy**: Bridges WebSocket connections to stdio-based ACP agents

## Architecture

```
┌─────────────┐                    ┌─────────────┐        ┌──────────────┐
│   iPhone    │◄──────────────────►│   Bridge    │◄──────►│  ACP Agent   │
│   App       │  WebSocket (LAN)   │   (Rust)    │  stdio │  (Copilot)   │
└─────────────┘                    └─────────────┘        └──────────────┘
     ▲                                     │
     │                                     │
     └─────────── QR Code Scan ───────────┘
              (local IP + port)
```

## Prerequisites

- Rust 1.70+ (install via [rustup](https://rustup.rs/))
- An ACP-compatible agent (e.g., GitHub Copilot with `--acp` flag)
- Mobile device on the same local network as your computer

## Installation

```bash
# Clone the repository
git clone https://github.com/aptove/bridge.git
cd bridge

# Build the tool
cargo build --release

# The binary is at target/release/acp-cloudflare-bridge
```

## Quick Start

### Step 1: Find Your Local IP

The bridge needs to know your local IP address for the QR code:

```bash
# macOS
ifconfig | grep "inet " | grep -v 127.0.0.1

# Linux
ip addr show | grep "inet " | grep -v 127.0.0.1
```

Note your local IP (e.g., `192.168.1.100`).

### Step 2: Start the Bridge

Start the bridge with your ACP agent command:

```bash
# For GitHub Copilot
./target/release/acp-cloudflare-bridge start \
  --agent-command "copilot --acp" \
  --port 8080 \
  --stdio-proxy

# For Gemini CLI
./target/release/acp-cloudflare-bridge start \
  --agent-command "gemini --experimental-acp" \
  --port 8080 \
  --stdio-proxy
```

The bridge will:
- Start a WebSocket server on port 8080
- Spawn the ACP agent process
- Display a QR code for mobile connection

### Step 3: Connect Your Mobile App

1. **Ensure same network**: Your phone must be on the same Wi-Fi network as your computer
2. **Open the Aptove app** on your iOS device
3. **Scan the QR code** displayed by the bridge
4. **Start chatting** with your local AI agent!

## Connection URL Format

The QR code contains a JSON payload:

```json
{
  "url": "ws://192.168.1.100:8080",
  "protocol": "acp",
  "version": "1.0",
  "authToken": "abc123...",
  "certFingerprint": "AB:CD:EF:12:34:..."
}
```

Your mobile app connects via WebSocket to the local IP and port, providing the auth token for authentication. The certificate fingerprint is used to validate the self-signed TLS certificate.

## Command Options

### `start`

Start the WebSocket bridge server:

```bash
bridge start [OPTIONS]
```

**Options:**

| Option | Description | Default |
|--------|-------------|---------|
| `--agent-command <CMD>` | Command to spawn the ACP agent | Required |
| `--port <PORT>` | Local WebSocket port | `8080` |
| `--bind <ADDR>` | Address to bind (use `127.0.0.1` for localhost only) | `0.0.0.0` |
| `--stdio-proxy` | Enable stdio proxy mode (bypasses Cloudflare) | Required for local use |
| `--qr` | Display QR code for mobile connection | Off |
| `--no-auth` | Disable authentication (NOT recommended) | Auth enabled |
| `--no-tls` | Disable TLS encryption (NOT recommended) | TLS enabled |
| `--max-connections-per-ip <N>` | Maximum concurrent connections per IP | `3` |
| `--max-attempts-per-minute <N>` | Maximum connection attempts per minute per IP | `10` |
| `--verbose` | Enable verbose logging | Off |

### Examples

```bash
# GitHub Copilot with QR code (TLS enabled by default)
./target/release/bridge start \
  --agent-command "copilot --acp" \
  --port 8080 \
  --stdio-proxy \
  --qr

# Bind to localhost only (more secure)
./target/release/bridge start \
  --agent-command "copilot --acp" \
  --bind 127.0.0.1 \
  --port 8080 \
  --stdio-proxy

# Development mode without TLS (NOT recommended)
./target/release/bridge start \
  --agent-command "copilot --acp" \
  --port 8080 \
  --stdio-proxy \
  --no-tls

# Custom agent with arguments
./target/release/bridge start \
  --agent-command "/path/to/my-agent --verbose" \
  --port 9000 \
  --stdio-proxy
```

## Troubleshooting

### Mobile app can't connect

1. **Check network**: Ensure phone and computer are on the same Wi-Fi
2. **Check firewall**: Allow incoming connections on port 8080
   ```bash
   # macOS - temporarily disable firewall or add exception
   # Check System Preferences → Security & Privacy → Firewall
   ```
3. **Verify IP**: Make sure the QR code contains your current local IP
4. **Test locally**: Try connecting from your computer first:
   ```bash
   websocat ws://localhost:8080
   ```

### Agent process fails to start

Test your agent command manually:

```bash
# Test Copilot
copilot --acp

# Test Gemini
gemini --experimental-acp
```

Ensure the agent accepts stdin and produces stdout in JSON-RPC format.

### Connection drops frequently

- Check your Wi-Fi stability
- Try moving closer to your router
- Ensure no VPN is interfering with local network traffic

## Security

### TLS Encryption

The bridge automatically generates a self-signed TLS certificate on first run:
- Certificate and key are stored in the config directory with restricted permissions (0600)
- The certificate fingerprint (SHA256) is included in the QR code
- Mobile apps validate the certificate fingerprint to prevent MITM attacks
- Valid for 1 year, includes SANs for localhost and your local network IP

**To disable TLS** (not recommended):
```bash
./target/release/bridge start --agent-command "copilot --acp" --stdio-proxy --no-tls
```

### Authentication

The bridge generates a unique authentication token on first run. This token:
- Is included in the QR code automatically
- Must be provided by mobile apps in the `X-Bridge-Token` header (or `?token=` query parameter)
- Is stored securely with restricted file permissions (0600 on Unix)

**To disable authentication** (not recommended):
```bash
./target/release/bridge start --agent-command "copilot --acp" --stdio-proxy --no-auth
```

### Network Security

⚠️ **Local Network Only**: This bridge is designed for local development and personal use. 

**Best practices:**
- Use `--bind 127.0.0.1` to restrict to localhost when possible
- Only use on trusted networks (your home Wi-Fi)
- Don't expose the bridge port to the internet
- Stop the bridge when not in use
- Keep the auth token secret - anyone with it can execute commands via your agent

### Rate Limiting

The bridge includes built-in rate limiting to prevent abuse:
- **Concurrent connections**: Max 3 connections per IP (configurable with `--max-connections-per-ip`)
- **Connection attempts**: Max 10 attempts per minute per IP (configurable with `--max-attempts-per-minute`)

Connections exceeding these limits are automatically rejected.

### Config File Location

Configuration, auth token, and TLS certificates are stored at:
- **macOS**: `~/Library/Application Support/com.bridge.bridge/`
- **Linux**: `~/.config/bridge/`

Files:
- `config.json` - Configuration and auth token
- `cert.pem` - TLS certificate
- `key.pem` - TLS private key

All files are created with restrictive permissions (0600) to protect sensitive data.

## Development

### Project Structure

```
src/
├── main.rs           # CLI entry point and command routing
├── bridge.rs         # WebSocket ↔ stdio bridge
├── config.rs         # Configuration management
└── qr.rs            # QR code generation
```

### Building for Release

```bash
cargo build --release
# Binary: target/release/acp-cloudflare-bridge
```

### Running Tests

```bash
cargo test
```

## License

Apache 2.0 License - see [LICENSE](LICENSE) for details

## Related Projects

- [ACP Swift SDK](https://github.com/aptove/swift-sdk) - Swift SDK for ACP clients
- [Gemini CLI](https://github.com/google/gemini-cli) - Google's ACP agent
- [GitHub Copilot](https://github.com/features/copilot) - GitHub's AI assistant with ACP support

---

**Questions or Issues?** Open an issue on GitHub

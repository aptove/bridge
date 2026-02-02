# ACP Bridge

A bridge between stdio-based Agent Client Protocol (ACP) agents and mobile applications.

## Transport Modes

| Mode | Use Case | Documentation |
|------|----------|---------------|
| **Local** | Same Wi-Fi network, secure pairing with QR code | [docs/transport/local.md](docs/transport/local.md) |
| **Cloudflare** | Remote access via Cloudflare Zero Trust | *Experimental* |

## Features

- 📱 **QR Code Pairing**: Secure one-time code pairing via QR scan
- 🔒 **TLS + Certificate Pinning**: Self-signed certificates with fingerprint validation
- ⚡ **WebSocket Streaming**: Real-time bidirectional communication
- 🦀 **Rust Performance**: Low-latency, high-throughput implementation

## Quick Start

```bash
# Build
cargo build --release

# Start with GitHub Copilot
./target/release/acp-cloudflare-bridge start \
  --agent-command "copilot --acp" \
  --port 8080 \
  --stdio-proxy \
  --qr
```

Scan the QR code with the Aptove iOS app to connect.

📖 **For detailed setup, security information, and troubleshooting, see [Local Transport Documentation](docs/transport/local.md).**

## Architecture

```
┌─────────────┐                    ┌─────────────┐        ┌──────────────┐
│   iPhone    │◄──────────────────►│   Bridge    │◄──────►│  ACP Agent   │
│   App       │  WebSocket (LAN)   │   (Rust)    │  stdio │  (Copilot)   │
└─────────────┘                    └─────────────┘        └──────────────┘
```

## Command Options

| Option | Description | Default |
|--------|-------------|---------|
| `--agent-command <CMD>` | Command to spawn the ACP agent | Required |
| `--port <PORT>` | Local WebSocket port | `8080` |
| `--bind <ADDR>` | Address to bind | `0.0.0.0` |
| `--stdio-proxy` | Enable stdio proxy mode | Required |
| `--qr` | Display QR code for pairing | Off |
| `--no-auth` | Disable authentication | Auth enabled |
| `--no-tls` | Disable TLS encryption | TLS enabled |
| `--max-connections-per-ip <N>` | Max concurrent connections per IP | `3` |
| `--max-attempts-per-minute <N>` | Max connection attempts per minute per IP | `10` |
| `--config-dir <PATH>` | Custom configuration directory | System default |

## Config Location

Default locations (can be overridden with `--config-dir`):
- **macOS**: `~/Library/Application Support/com.bridge.bridge/`
- **Linux**: `~/.config/bridge/`

Example with custom config directory:
```bash
./target/release/bridge --config-dir ./my-config start --agent-command "copilot --acp" --qr
```

## Development

```bash
cargo build --release    # Build
cargo test               # Run tests
```

## License

Apache 2.0 - see [LICENSE](LICENSE)

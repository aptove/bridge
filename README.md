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

# Start with session persistence (keep agent alive on disconnect)
./target/release/acp-cloudflare-bridge start \
  --agent-command "copilot --acp" \
  --port 8080 \
  --stdio-proxy \
  --qr \
  --keep-alive \
  --session-timeout 3600
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
| `--keep-alive` | Keep agent processes alive when clients disconnect | Off |
| `--session-timeout <SECS>` | Idle timeout before killing disconnected agents | `1800` (30 min) |
| `--max-agents <N>` | Maximum concurrent agent processes | `10` |
| `--buffer-messages` | Buffer agent messages while client is disconnected | Off |
| `--config-dir <PATH>` | Custom configuration directory | System default |

## Session Persistence (Keep-Alive)

By default, the bridge kills the agent process when a client disconnects. With `--keep-alive`, agent processes remain alive during temporary disconnections (network switches, app backgrounding), enabling seamless session resumption.

### How It Works

1. Client connects → Bridge looks up existing agent by auth token, or spawns new
2. Client disconnects → Agent stays alive in the pool, enters idle state
3. Client reconnects → Bridge reattaches to the same agent process
4. Idle timeout → Agent is terminated after configurable period of no client

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      AGENT POOL                             │
│  ┌──────────────────────────────────────────────────────┐   │
│  │  token_abc → Agent Process [connected]               │   │
│  │  token_xyz → Agent Process [idle: 5min]              │   │
│  └──────────────────────────────────────────────────────┘   │
│  Reaper task: checks every 60s, kills idle agents           │
└─────────────────────────────────────────────────────────────┘
```

### Configuration

| Flag | Description | Default |
|------|-------------|---------|
| `--keep-alive` | Enable session persistence | Off |
| `--session-timeout <secs>` | How long idle agents stay alive | 1800 (30 min) |
| `--max-agents <n>` | Max concurrent agents in pool | 10 |
| `--buffer-messages` | Buffer agent output during disconnect | Off |

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

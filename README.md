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

## Troubleshooting

### Session Persistence Issues

| Symptom | Cause | Fix |
|---------|-------|-----|
| Agent not reused after reconnect | Auth token mismatch between connections | Ensure the mobile app sends the same `X-Bridge-Token` header on reconnect |
| Agent killed immediately on disconnect | `--keep-alive` flag not set | Add `--keep-alive` to the start command |
| "Agent pool is full" error | All agent slots occupied by connected clients | Increase `--max-agents` or disconnect unused sessions |
| Messages lost during disconnect | Message buffering not enabled | Add `--buffer-messages` flag |
| Agent dies while client is away | Process crashed or idle timeout too short | Increase `--session-timeout`; check agent stderr logs with `--verbose` |
| Reconnect works but state is lost | Agent process itself doesn't persist state | This is agent-specific; the bridge preserves the *process*, not internal state |

### Connection Issues

| Symptom | Cause | Fix |
|---------|-------|-----|
| QR code not scanning | Phone camera can't read terminal QR | Try `--qr` flag to save as image file |
| "Unauthorized" on connect | Wrong or missing auth token | Re-scan QR code; check `X-Bridge-Token` header |
| TLS handshake failure | Certificate mismatch | Delete config dir and restart to regenerate certs; re-pair device |
| "Rate limit exceeded" | Too many connection attempts | Wait 60 seconds; adjust `--max-attempts-per-minute` |
| Connection drops on Wi-Fi switch | TCP connection broken by network change | Enable `--keep-alive` for automatic session resumption |

### Debugging

```bash
# Enable verbose logging to see all bridge activity
./target/release/bridge start --agent-command "copilot --acp" --stdio-proxy --verbose

# Check pool stats in verbose mode (logged every 60s when agents exist)
# Look for: "AgentPool stats: 2/10 agents (1 connected, 1 idle)"

# Test agent command independently
echo '{"jsonrpc":"2.0","method":"initialize","id":1}' | copilot --acp
```

## Security Considerations

### Authentication

- **Always use authentication in production.** The `--no-auth` flag is for development only.
- Auth tokens are generated using cryptographically secure random bytes (32 bytes, hex-encoded).
- Tokens are persisted in the bridge config file — protect this file with appropriate permissions.
- The token is transmitted via the QR code during initial pairing and stored securely on the mobile device.

### TLS / Certificate Pinning

- The bridge generates self-signed TLS certificates on first run.
- A SHA-256 certificate fingerprint is included in the QR code for pinning.
- Mobile clients validate the fingerprint on every connection, preventing MITM attacks.
- Use `--no-tls` **only** for local development behind a trusted network.

### Session Persistence Security

- **Token-based routing**: Agent processes are keyed by auth token. A valid token is required to reconnect to an existing session.
- **No cross-session access**: Each token maps to exactly one agent process. Clients cannot access other sessions.
- **Idle timeout**: Disconnected agents are automatically terminated after `--session-timeout` seconds, limiting the window for stale sessions.
- **Max-agents limit**: Prevents resource exhaustion by capping the number of concurrent agent processes.
- **Process isolation**: Each agent runs as a separate OS process with its own stdin/stdout.

### Rate Limiting

- Per-IP connection limits prevent brute-force and denial-of-service attacks.
- Default: 3 concurrent connections and 10 attempts per minute per IP.
- Pairing codes are single-use and rate-limited to prevent enumeration.

### Recommendations

1. Always run with TLS enabled (default).
2. Use `--keep-alive` with a reasonable `--session-timeout` (default 30 min).
3. Set `--max-agents` to match expected concurrent users.
4. Restrict `--bind` to `127.0.0.1` if only local connections are needed.
5. Rotate auth tokens periodically by deleting the config and re-pairing.

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

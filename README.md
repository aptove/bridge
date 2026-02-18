# ACP Bridge

A bridge between stdio-based Agent Client Protocol (ACP) agents and mobile applications.

## Transport Modes

| Mode | Use Case | Documentation |
|------|----------|---------------|
| **Local** | Same Wi-Fi network, secure pairing with QR code | [docs/transport/local.md](docs/transport/local.md) |
| **Cloudflare** | Remote access via Cloudflare Zero Trust (internet-accessible) | [docs/transport/cloudflare.md](docs/transport/cloudflare.md) |
| **Tailscale** | Private overlay network (serve: MagicDNS+HTTPS; ip: direct Tailscale IP) | [docs/transport/tailscale.md](docs/transport/tailscale.md) |

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

With `--keep-alive`, agent processes remain alive during temporary disconnections (network switches, app backgrounding), enabling seamless session resumption. The bridge intercepts re-initialization requests on reconnect to preserve full conversation context.

📖 **For detailed architecture, reconnection flow, and troubleshooting, see [Persistent Sessions Documentation](docs/session/persistent-session.md).**

## Troubleshooting

> **Session persistence issues?** See the [Persistent Sessions Troubleshooting](docs/session/persistent-session.md#troubleshooting) guide.

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

Sessions are isolated by auth token with idle timeouts and max-agent limits. See [Persistent Sessions — Security](docs/session/persistent-session.md#security) for details.

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

The bridge stores configuration in `config.json` which includes:
- TLS certificate fingerprint
- Authentication token (relay token)
- Connection settings

### Default Paths

Determined by the `directories` crate with package identifier `com.bridge.bridge`:

- **macOS**: `~/Library/Application Support/com.bridge.bridge/config.json`
- **Linux**: `~/.config/bridge/config.json`
- **Windows**: `%APPDATA%\bridge\bridge\config\config.json`

### Custom Location

Override with `--config-dir`:
```bash
./target/release/bridge --config-dir ./my-config start --agent-command "copilot --acp" --qr
```

### Rotating Credentials

To generate a new relay token and invalidate old device registrations:

```bash
# Delete config file
rm -f ~/Library/Application\ Support/com.bridge.bridge/config.json  # macOS
rm -f ~/.config/bridge/config.json                                   # Linux

# Restart bridge - generates new token
./target/release/bridge start --agent-command "copilot --acp" --qr --verbose
```

The new relay token will be printed with `--verbose` mode.

**Note**: Deleting the config also regenerates TLS certificates, requiring device re-pairing.

## Claude Code Integration

echo '{"model": "sonnet"}' > ~/.claude/settings.json

# Then run bridge
export ANTHROPIC_API_KEY=sk-ant-your-key
./target/release/bridge start \
  --agent-command "claude-code-acp" \
  --port 8080 \
  --stdio-proxy \
  --qr \
  --keep-alive \
  --verbose \
  --session-timeout 3600
```

## Development

```bash
cargo build --release    # Build
cargo test               # Run tests
```

## License

Apache 2.0 - see [LICENSE](LICENSE)

# ACP Bridge

A bridge between stdio-based Agent Client Protocol (ACP) agents and mobile applications.

## Transport Modes

| Mode | Use Case | Documentation |
|------|----------|---------------|
| **Local** | Same Wi-Fi network, secure pairing with QR code | [docs/transport/local.md](docs/transport/local.md) |
| **Cloudflare** | Remote access via Cloudflare Zero Trust (internet-accessible) | [docs/transport/cloudflare.md](docs/transport/cloudflare.md) |
| **Tailscale Serve** | Private overlay network via MagicDNS + HTTPS | [docs/transport/tailscale.md](docs/transport/tailscale.md) |
| **Tailscale IP** | Direct Tailscale IP with self-signed TLS | [docs/transport/tailscale.md](docs/transport/tailscale.md) |

Multiple transports can run simultaneously — the bridge runs one listener per enabled transport.

## Features

- 📱 **QR Code Pairing**: Secure one-time code pairing via QR scan
- 🔒 **TLS + Certificate Pinning**: Self-signed certificates with fingerprint validation
- ⚡ **WebSocket Streaming**: Real-time bidirectional communication
- 🌐 **Multi-Transport**: Local, Cloudflare, Tailscale — all at once
- 🔑 **Stable Agent Identity**: `agent_id` UUID persisted in `common.toml` for multi-transport dedup on mobile
- 🦀 **Rust Performance**: Low-latency, high-throughput implementation

## Quick Start

```bash
# Build
cargo build --release

# Start with local transport (default — no config needed)
./target/release/bridge run \
  --agent-command "gemini --experimental-acp" \
  --qr
```

Scan the QR code with the Aptove mobile app to connect.

## Configuration — `common.toml`

All transport settings live in `common.toml`. The file is created automatically with local transport enabled on first run.

**Default location** (depends on how you run the bridge):

| Runtime | macOS | Linux |
|---------|-------|-------|
| `aptove run` (embedded) | `~/Library/Application Support/Aptove/common.toml` | `~/.config/Aptove/common.toml` |
| `bridge` (standalone) | `~/Library/Application Support/com.aptove.bridge/common.toml` | `~/.config/bridge/common.toml` |

When using `aptove run`, this config is shared across all workspaces.

Override with `--config-dir`:
```bash
bridge --config-dir ./my-config start --agent-command "gemini --experimental-acp"
```

### Example `common.toml`

```toml
agent_id  = "550e8400-e29b-41d4-a716-446655440000"  # auto-generated UUID
auth_token = "base64urltoken"                        # auto-generated

[transports.local]
enabled = true
port    = 8765
tls     = true

[transports.cloudflare]
enabled       = true
hostname      = "https://agent.example.com"
tunnel_id     = "abc123"
tunnel_secret = "..."
account_id    = "..."
client_id     = "client.access"
client_secret = "xxxxx"

[transports.tailscale-serve]
enabled = true

[transports.tailscale-ip]
enabled = true
port    = 8765
tls     = true
```

Enable only the transports you need. `agent_id` and `auth_token` are generated automatically on first run and stay stable across restarts.

## Commands

### `start` — Run the bridge

```bash
bridge run --agent-command "<your-agent-command>"
```

| Flag | Description | Default |
|------|-------------|---------|
| `--agent-command <CMD>` | Command to spawn the ACP agent | Required |
| `--bind <ADDR>` | Address to bind all listeners | `0.0.0.0` |
| `--qr` | Display QR code(s) for pairing at startup | Off |
| `--verbose` | Enable info-level logging | Off (warn only) |

Transport selection, ports, TLS, and auth tokens are all read from `common.toml`.

### `show-qr` — Show QR code for a second device

```bash
bridge show-qr
```

Displays the connection QR code for the currently active transport. The bridge must already be running. Use this to pair an additional device without restarting the bridge.

To show the QR at initial startup, pass `--qr` to `bridge run` instead:

```bash
bridge run --agent-command "aptove stdio" --qr
```

### `setup` — Provision Cloudflare infrastructure

```bash
bridge setup \
  --api-token  "your-api-token" \
  --account-id "your-account-id" \
  --domain     "example.com" \
  --subdomain  "agent"
```

Creates the Cloudflare tunnel, DNS record, Access Application, and Service Token. Saves credentials to `common.toml` under `[transports.cloudflare]`. Only needed once.

| Flag | Description | Default |
|------|-------------|---------|
| `--api-token <TOKEN>` | Cloudflare API token | Required |
| `--account-id <ID>` | Cloudflare account ID | Required |
| `--domain <DOMAIN>` | Domain managed by Cloudflare | Required |
| `--subdomain <SUB>` | Subdomain for the bridge endpoint | `agent` |
| `--tunnel-name <NAME>` | Name for the Cloudflare tunnel | `aptove-tunnel` |

### `status` — Check configuration

```bash
bridge status
```

Prints the active `common.toml` path, `agent_id`, enabled transports, and Tailscale availability.

## Architecture

```
┌─────────────┐                    ┌──────────────────────┐        ┌──────────────┐
│  Mobile App │◄──────────────────►│  Bridge (per-transport│◄──────►│  ACP Agent   │
│ (iOS/Android│  WebSocket (TLS)   │  local / CF / TS)    │  stdio │  (your cmd)  │
└─────────────┘                    └──────────────────────┘        └──────────────┘
```

Multiple transport listeners share the same agent process.

## Troubleshooting

### Connection Issues

| Symptom | Cause | Fix |
|---------|-------|-----|
| QR code not scanning | Phone camera can't read terminal QR | Increase terminal font size, or copy the pairing URL and open manually |
| "Unauthorized" on connect | Wrong or missing auth token | Re-scan QR code |
| TLS handshake failure | Certificate mismatch | Delete the config dir and restart to regenerate certs; re-pair the device |
| App cannot reach bridge | Firewall blocking the port | Check OS firewall; ensure the port in `common.toml` is open |
| All transports fail to start | `common.toml` has no `enabled = true` transport | Run `bridge status` to see configured transports |

### Debugging

```bash
# Enable verbose logging
bridge run --agent-command "gemini --experimental-acp" --verbose

# Check which transports are configured
bridge status

# Test agent command independently
echo '{"jsonrpc":"2.0","method":"initialize","id":1}' | gemini --experimental-acp
```

## Security

- **Auth token**: auto-generated 32-byte random value, stored in `common.toml` (`0600`). Transmitted to mobile during QR pairing and stored in the device Keychain.
- **TLS**: self-signed certificate generated on first run. Certificate fingerprint is included in the QR pairing payload and pinned by the mobile app to prevent MITM attacks.
- **Pairing codes**: 6-digit, single-use, expire after 60 seconds. Rate-limited to 5 attempts per code.
- **`common.toml`**: contains all secrets. Permissions are set to `0600` automatically. Keep it secure.

To rotate credentials (invalidates all paired devices):

```bash
# Using aptove run (embedded bridge):
rm ~/Library/Application\ Support/Aptove/common.toml   # macOS
rm ~/.config/Aptove/common.toml                        # Linux
aptove run --qr

# Using standalone bridge binary:
rm ~/Library/Application\ Support/com.aptove.bridge/common.toml   # macOS
rm ~/.config/bridge/common.toml                                    # Linux
bridge run --agent-command "aptove stdio" --qr
```

## Development

```bash
cargo build --release    # Build
cargo test               # Run tests
```

## License

Apache 2.0 — see [LICENSE](LICENSE)

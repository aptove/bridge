# Bridge

[![crates.io](https://img.shields.io/crates/v/aptove-bridge.svg)](https://crates.io/crates/aptove-bridge)
[![GitHub Release](https://img.shields.io/github/v/release/aptove/bridge?logo=github&label=download)](https://github.com/aptove/bridge/releases/latest)
[![npm](https://img.shields.io/npm/v/%40aptove%2Fbridge?logo=npm&label=npm)](https://www.npmjs.com/package/@aptove/bridge)
[![Discord](https://img.shields.io/badge/Discord-Join-5865F2?logo=discord&logoColor=white)](https://discord.gg/gD7AMxBy9y)

Aptove Bridge is a stand-alone app to connect any AI Agents that support Agent Client Protocol (ACP) with the Aptove Mobile App to seamlessly manage the AI agents running in a server or computer.

Bridge is actively developed and we are still experimenting. Until the first stable version, expect breaking changes in the configuration files and how it works.

## Transport Modes

| Mode | Use Case | Documentation |
|------|----------|---------------|
| **Local** | Same Wi-Fi network, secure pairing with QR code | [docs/transport/local.md](docs/transport/local.md) |
| **Cloudflare** | Remote access via Cloudflare Zero Trust (internet-accessible) | [docs/transport/cloudflare.md](docs/transport/cloudflare.md) |
| **Tailscale** | Private overlay network via MagicDNS + HTTPS (Recommended) | [docs/transport/tailscale.md](docs/transport/tailscale.md) |

## Features

- 📱 **QR Code Pairing**: Secure one-time code pairing via QR scan
- 🔒 **TLS + Certificate Pinning**: Self-signed certificates with fingerprint validation
- ⚡ **WebSocket Streaming**: Real-time bidirectional communication
- 🌐 **Multi-Transport**: Local, Cloudflare, Tailscale — configure and switch between them
- 🔑 **Stable Agent Identity**: `agent_id` UUID persisted in `common.toml` for multi-transport dedup on mobile
- 🔔 **Push Notifications**: Wake the mobile app when the agent responds while backgrounded
- 🖥️ **Terminal UI**: Interactive TUI for setup and day-to-day management
- 🦀 **Embeddable**: Use as a library with `StdioBridge` for in-process deployment

---

### Quick Start

```bash
# Build
cargo build --release

# Start the bridge — opens the interactive TUI (setup wizard on first run)
./target/release/bridge
```

On first launch the setup wizard runs automatically: it asks you to choose an agent command and a transport, then starts the bridge and shows a QR code for pairing.

Scan the QR code with the mobile app to connect.

### Configuration — `common.toml`

All settings live in `common.toml`, created automatically by the setup wizard on first run.

**Default location:** `.aptove-bridge/` directory inside the working directory where the bridge is launched.

#### Example `common.toml`

```toml
agent_id   = "550e8400-e29b-41d4-a716-446655440000"  # auto-generated UUID
auth_token = "base64urltoken"                         # auto-generated
agent_command = "copilot --acp"                       # set by setup wizard

keep_alive = true    # prevent system sleep while bridge is running
log_level  = "WARN"  # ERROR / WARN / INFO / DEBUG / TRACE

[transports.local]
enabled = true
port    = 8765
tls     = true

[transports.cloudflare]
enabled       = false
hostname      = "https://agent.example.com"
tunnel_id     = "abc123"
tunnel_secret = "..."
account_id    = "..."
client_id     = "client.access"
client_secret = "xxxxx"

[transports.tailscale-serve]
enabled = false

# Optional — enables push notifications (see Push Notifications section below)
[push_relay]
url           = "https://push.aptove.com"
token_url     = "https://token.aptove.com"
client_id     = "your-client-id"
client_secret = "your-client-secret"
```

No transports are pre-enabled. The setup wizard asks you to choose one on first run.

`agent_id` and `auth_token` are generated automatically on first run and stay stable across restarts.

#### Config Directory Files

All bridge state lives in the config directory. These files are created automatically on first run:

| File | Purpose |
|------|---------|
| `common.toml` | Main config — `agent_id`, `auth_token`, `agent_command`, and transport settings. Permissions `0600`. |
| `cert.pem` | Self-signed TLS certificate for the local transport WebSocket server. Its fingerprint is embedded in the QR pairing payload for certificate pinning. |
| `key.pem` | Private key for the TLS certificate. |
| `cert-extra-sans.json` | Tracks extra Subject Alternative Names (IPs/hostnames) baked into the TLS cert. When these change, the cert is automatically regenerated. |

### TUI

Running `bridge` opens the interactive TUI. On first launch it runs the setup wizard:

```
Select an agent to run:
  [1] Copilot  (copilot --acp)
  [2] Gemini   (gemini --experimental-acp)
  [3] Goose    (goose acp)
  [4] Custom
Enter number [1]:
```

Select **Custom** to enter any ACP-compatible agent command, for example:

```
npx @agentclientprotocol/claude-agent-acp
```

After the wizard completes the bridge starts and displays a QR code for pairing.

### TUI slash commands

Type these in the input bar while the bridge is running:

| Command | Description |
|---------|-------------|
| `/qr` | Show QR pairing code |
| `/agent` | Change the AI agent command |
| `/push` | Configure push notifications |
| `/test-push` | Send a test push notification |
| `/reconnect` | Restart the transport |
| `/keep-alive` | Toggle prevent-sleep (on by default) |
| `/log-level` | Change log verbosity (default: WARN) |
| `/clear-logs` | Clear the log view |
| `/copy-logs` | Copy all logs to clipboard |
| `/help` | List all commands |
| `/quit` | Exit the bridge |

---

## Push Notifications

The bridge can alert the mobile app when the agent produces output while the app is backgrounded (WebSocket disconnected). The bridge never holds APNs/FCM credentials — it authenticates to a push relay service using a short-lived RS256 JWT and the relay handles APNs/FCM delivery.

### How it works

```
Mobile app (backgrounded)          Bridge                      Push relay
                                                               (push.aptove.com)
        │                             │                              │
        │  [app backgrounds]          │                              │
        │                             │                              │
        │                             │  agent produces output       │
        │                             │◄─────────────────────       │
        │                             │                              │
        │                             │  no WebSocket receiver       │
        │                             │  (broadcast send fails)      │
        │                             │                              │
        │                             │  POST /push (JWT Bearer) ───►│
        │                             │                              │
        │◄────────────────────────── APNs/FCM notification          │
        │                             │                              │
        │  [app foregrounds]          │                              │
        │  reconnects WebSocket ─────►│                              │
        │◄── buffered messages ───────│                              │
```

### Trigger logic

Push is triggered by **passive failure detection** — the bridge never polls for connectivity:

| Site | Trigger | Source |
|------|---------|--------|
| Agent pool (main path) | Agent stdout arrives; `broadcast::Sender::send()` returns `Err` (zero receivers — no WebSocket client connected) | `src/agent_pool.rs` |
| Active connection drop | `ws_sender.send()` returns `Err` (client disconnected mid-stream); message is buffered before notify | `src/bridge.rs` |

A 30-second per-client debounce prevents notification storms when the agent is verbose.

### Setup

Configure push via the `/push` TUI command or add the `[push_relay]` section directly to `common.toml`:

```toml
[push_relay]
url           = "https://push.aptove.com"   # Push relay base URL
token_url     = "https://token.aptove.com"  # JWT token service URL
client_id     = "your-client-id"            # M2M client ID (JWT sub)
client_secret = "your-client-secret"        # M2M client secret
```

All four fields are required. If the section is absent or any field is empty, push is silently disabled. The push relay URL is included in the QR pairing payload so the mobile app knows where to register its device token.

### Security

- Device tokens travel over the bridge's authenticated WebSocket (bearer `authToken` + TLS) — the same channel that carries the full LLM conversation.
- JWT credentials (`client_id`, `client_secret`) never leave the bridge process.
- The relay isolates registered devices per JWT `sub` (`client_id`) — one bridge cannot trigger pushes for another bridge's devices.
- Notification content is fixed (`"Your agent has new activity"`) to prevent leaking agent response text.

---

## Architecture

```
┌─────────────┐                    ┌───────────────────────┐        ┌──────────────┐
│  Client App │◄──────────────────►│  bridge binary        │◄──────►│  ACP Agent   │
│ (iOS/Android│  WebSocket (TLS)   │  (transport listener) │  stdio │  (your cmd)  │
└─────────────┘                    └───────────────────────┘        └──────────────┘
```

The agent process is spawned as a subprocess via stdio — `StdioBridge` handles the WebSocket server, TLS, auth, and pairing in-process alongside your application.

---

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| QR code not scanning | Phone camera can't read terminal QR | Increase terminal font size, or copy the pairing URL and open manually |
| "Unauthorized" on connect | Wrong or missing auth token | Re-scan QR code |
| TLS handshake failure | Certificate mismatch | Delete the config dir and restart to regenerate certs; re-pair the device |
| App cannot reach bridge | Firewall blocking the port | Check OS firewall; ensure the port in `common.toml` is open |
| Transport fails to start | No transport has `enabled = true` | Run the bridge and check the setup wizard |

### Debugging

```bash
# Change log verbosity via TUI
# Type /log-level and select DEBUG or TRACE

# Or set it permanently in common.toml
log_level = "DEBUG"

# Test agent command independently
echo '{"jsonrpc":"2.0","method":"initialize","id":1}' | copilot --acp
```

### Testing with Other ACP-Compatible Agents

Please note that the project is extensively tested only with Copilot CLI. Open a bug report if you notice any issues with other ACP-Compatible Agents.

Run `bridge` and pick an agent from the setup wizard, or edit `agent_command` directly in `common.toml`.

---

## Security

- **Auth token**: auto-generated 32-byte random value, stored in `common.toml` (`0600`). Transmitted to mobile during QR pairing and stored in the device Keychain.
- **TLS**: self-signed certificate generated on first run. Certificate fingerprint is included in the QR pairing payload and pinned by the mobile app to prevent MITM attacks.
- **Pairing codes**: 6-digit, single-use, expire after 60 seconds. Rate-limited to 5 attempts per code.
- **`common.toml`**: contains all secrets. Permissions are set to `0600` automatically. Keep it secure.
- **Agent command**: saved to `agent_command` in `common.toml` by the setup wizard. It can be changed at any time via the `/agent` TUI command. The bridge is an operator tool: whoever can invoke it already has local shell access, so the agent command is implicitly trusted to the same degree as any other command that user could run.

To rotate credentials (invalidates all paired devices):

```bash
rm .aptove-bridge/common.toml
bridge
```

---

## Development

```bash
cargo build --release    # Build
cargo test               # Run tests
```

## License

Apache License 2.0 — see [LICENSE](LICENSE)

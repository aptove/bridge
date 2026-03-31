# Bridge

[![crates.io](https://img.shields.io/crates/v/aptove-bridge.svg)](https://crates.io/crates/aptove-bridge)
[![GitHub Release](https://img.shields.io/github/v/release/aptove/bridge?logo=github&label=download)](https://github.com/aptove/bridge/releases/latest)
[![npm](https://img.shields.io/npm/v/%40aptove%2Fbridge?logo=npm&label=npm)](https://www.npmjs.com/package/@aptove/bridge)
[![Discord](https://img.shields.io/badge/Discord-Join-5865F2?logo=discord&logoColor=white)](https://discord.gg/gD7AMxBy9y)

A bridge library and application between Agent Client Protocol (ACP) agents and clients.

`aptove-bridge` can be used in two ways:

- **Standalone binary** — run `bridge` as a separate process that spawns your ACP agent over stdio and exposes it over WebSocket to mobile or desktop clients.
- **Embedded library** — add `aptove-bridge` as a Rust dependency and run the bridge server in-process alongside your agent, with no subprocess or stdio pipe required.

## Transport Modes

| Mode | Use Case | Documentation |
|------|----------|---------------|
| **Local** | Same Wi-Fi network, secure pairing with QR code | [docs/transport/local.md](docs/transport/local.md) |
| **Cloudflare** | Remote access via Cloudflare Zero Trust (internet-accessible) | [docs/transport/cloudflare.md](docs/transport/cloudflare.md) |
| **Tailscale Serve** | Private overlay network via MagicDNS + HTTPS | [docs/transport/tailscale.md](docs/transport/tailscale.md) |
| **Tailscale IP** | Direct Tailscale IP with self-signed TLS | [docs/transport/tailscale.md](docs/transport/tailscale.md) |

One transport is active at a time. When multiple are enabled in `common.toml`, the bridge prompts you to select one at startup.

## Features

- 📱 **QR Code Pairing**: Secure one-time code pairing via QR scan
- 🔒 **TLS + Certificate Pinning**: Self-signed certificates with fingerprint validation
- ⚡ **WebSocket Streaming**: Real-time bidirectional communication
- 🌐 **Multi-Transport**: Local, Cloudflare, Tailscale — configure and switch between them
- 🔑 **Stable Agent Identity**: `agent_id` UUID persisted in `common.toml` for multi-transport dedup on mobile
- 🦀 **Embeddable**: Use as a library with `BridgeServer` for in-process deployment

---

## Using as a Library

Add to your `Cargo.toml`:

```toml
[dependencies]
aptove-bridge = "0.1"
```

### Minimal Example

```rust
use aptove_bridge::{BridgeServer, BridgeServeConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Build from defaults — reads transport selection from common.toml.
    // Prompts the user to select a transport if multiple are enabled.
    let mut server = BridgeServer::build(&BridgeServeConfig::default())?;

    // Display a pairing QR code so a client can connect
    server.show_qr()?;

    // Take the in-process transport — wire it to your agent message loop
    let mut transport = server.take_transport();

    // Run your agent loop and bridge listener concurrently
    tokio::select! {
        _ = my_agent_loop(&mut transport) => {}
        res = server.start() => { res?; }
    }

    Ok(())
}
```

`take_transport()` returns an `InProcessTransport` that implements the same `Transport` trait as `StdioTransport` — your agent message loop works identically whether running standalone or embedded.

### `BridgeServeConfig`

| Field | Default | Description |
|-------|---------|-------------|
| `port` | `8080` | WebSocket listen port (overridden by `common.toml` per-transport config) |
| `bind_addr` | `"0.0.0.0"` | Bind address |
| `tls` | `true` | Enable TLS (self-signed cert auto-generated) |
| `auth_token` | `None` | Bearer token required for connections (auto-loaded from `common.toml`) |
| `keep_alive` | `false` | Enable keep-alive agent pool |
| `config_dir` | platform default | Directory for `common.toml`, TLS certs, and credentials |

Load from disk (reads `bridge.toml` and `common.toml`, generates `agent_id` if absent):

```rust
let config = BridgeServeConfig::load()?;
```

---

## Standalone Binary

### Quick Start

```bash
# Build
cargo build --release

# Start the bridge — interactive agent selection menu
./target/release/bridge

# Or specify the agent directly
./target/release/bridge run --agent-command "copilot --acp"
```

Running `bridge` with no subcommand defaults to `run`. When `--agent-command` is omitted, an interactive menu lets you pick from known agents (Copilot, Gemini, Goose) or enter a custom command.

Scan the QR code with the mobile app to connect.

### Configuration — `common.toml`

All transport settings live in `common.toml`. The file is created automatically with local transport enabled on first run.

**Default location:**

| Platform | Path |
|----------|------|
| macOS | `~/Library/Application Support/com.aptove.bridge/common.toml` |
| Linux | `~/.config/bridge/common.toml` |

Override with `-c` / `--config-dir`:
```bash
bridge -c ./my-config run --agent-command "copilot --acp"
```

#### Example `common.toml`

```toml
agent_id   = "550e8400-e29b-41d4-a716-446655440000"  # auto-generated UUID
auth_token = "base64urltoken"                         # auto-generated

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

### Commands

#### `run` — Start the bridge (default)

`run` is the default subcommand — running `bridge` with no arguments is equivalent to `bridge run`.

```bash
# Interactive agent selection
bridge

# Or specify the agent directly
bridge run --agent-command "copilot --acp"
```

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--agent-command <CMD>` | `-a` | Command to spawn the ACP agent | Interactive menu |
| `--bind <ADDR>` | `-b` | Address to bind the listener | `0.0.0.0` |
| `--verbose` | | Enable info-level logging | Off (warn only) |
| `--advertise-addr <ADDR>` | | Override LAN address in QR pairing URL | Auto-detected |

When `--agent-command` is omitted, the bridge presents an interactive menu:

```
Select an agent to run:
  [1] Copilot  (copilot --acp)
  [2] Gemini   (gemini --experimental-acp)
  [3] Goose    (goose acp)
  [4] Custom
Enter number [1]:
```

The QR code for pairing is always displayed at startup.

Transport selection, port, TLS, and auth token are all read from `common.toml`.

#### `show-qr` — Show QR code for a second device

```bash
bridge show-qr
```

Displays the connection QR code for the currently active transport. The bridge must already be running. Use this to pair an additional device without restarting.

#### `setup` — Provision Cloudflare infrastructure

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
| `--tunnel-name <NAME>` | Name for the Cloudflare tunnel | `bridge-tunnel` |

#### `status` — Check configuration

```bash
bridge status
```

Prints the active `common.toml` path, `agent_id`, enabled transports, and Tailscale availability.

---

## Architecture

### Standalone

```
┌─────────────┐                    ┌───────────────────────┐        ┌──────────────┐
│  Client App │◄──────────────────►│  bridge binary        │◄──────►│  ACP Agent   │
│ (iOS/Android│  WebSocket (TLS)   │  (transport listener) │  stdio │  (your cmd)  │
└─────────────┘                    └───────────────────────┘        └──────────────┘
```

### Embedded (library)

```
┌─────────────┐                    ┌──────────────────────────────────────────────┐
│  Client App │◄──────────────────►│  Your process                                │
│ (iOS/Android│  WebSocket (TLS)   │  ┌─────────────────┐  ┌────────────────────┐ │
└─────────────┘                    │  │  BridgeServer   │◄►│  Agent message loop│ │
                                   │  │  (transport)    │  │ (InProcessTransport│ │
                                   │  └─────────────────┘  └────────────────────┘ │
                                   └──────────────────────────────────────────────┘
```

No subprocess is spawned. Agent and bridge communicate via in-process channels.

---

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| QR code not scanning | Phone camera can't read terminal QR | Increase terminal font size, or copy the pairing URL and open manually |
| "Unauthorized" on connect | Wrong or missing auth token | Re-scan QR code |
| TLS handshake failure | Certificate mismatch | Delete the config dir and restart to regenerate certs; re-pair the device |
| App cannot reach bridge | Firewall blocking the port | Check OS firewall; ensure the port in `common.toml` is open |
| Transport fails to start | `common.toml` has no `enabled = true` transport | Run `bridge status` to see configured transports |

### Debugging

```bash
# Enable verbose logging
bridge run --verbose

# Check which transports are configured
bridge status

# Test agent command independently
echo '{"jsonrpc":"2.0","method":"initialize","id":1}' | copilot --acp
```

### Testing with Other ACP-Compatible Agents

Please note that the project is extensively tested only with Copilot CLI. Open a bug report if you notice any issues with other ACP-Compatible Agents.

The easiest way to test any supported agent is to run `bridge` and pick it from the interactive menu. Alternatively, pass the command directly:

```bash
bridge run -a "gemini --experimental-acp" --verbose
bridge run -a "goose acp" --verbose
```

---

## Security

- **Auth token**: auto-generated 32-byte random value, stored in `common.toml` (`0600`). Transmitted to mobile during QR pairing and stored in the device Keychain.
- **TLS**: self-signed certificate generated on first run. Certificate fingerprint is included in the QR pairing payload and pinned by the mobile app to prevent MITM attacks.
- **Pairing codes**: 6-digit, single-use, expire after 60 seconds. Rate-limited to 5 attempts per code.
- **`common.toml`**: contains all secrets. Permissions are set to `0600` automatically. Keep it secure.

To rotate credentials (invalidates all paired devices):

```bash
# macOS
rm ~/Library/Application\ Support/com.aptove.bridge/common.toml
# Linux
rm ~/.config/bridge/common.toml

bridge
```

---

## Development

```bash
cargo build --release    # Build
cargo test               # Run tests
```

## License

Apache 2.0 — see [LICENSE](LICENSE)

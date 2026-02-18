# Cloudflare Zero Trust Transport

This guide covers configuring the bridge to accept connections from iOS and Android apps over the internet using Cloudflare Zero Trust. This is the recommended transport for production deployments where devices cannot be on the same local network as the bridge.

## How It Works

```
┌─────────────────┐        Internet         ┌───────────────────┐
│   iPhone/Android│◄───────────────────────►│  Cloudflare Edge  │
│   App           │  wss://agent.example.com│                   │
└─────────────────┘                         └────────┬──────────┘
     Sends CF-Access-Client-Id header                │ cloudflared tunnel
     Sends CF-Access-Client-Secret header            │ (outbound, no port-forward needed)
                                                     ▼
                                          ┌─────────────────────┐       ┌──────────────┐
                                          │  Bridge (Rust)      │◄─────►│  ACP Agent   │
                                          │  localhost:8080      │ stdio │  (e.g. Copilot)│
                                          └─────────────────────┘       └──────────────┘
```

1. **`bridge setup`** provisions Cloudflare infrastructure via API: a tunnel, a DNS record, a Zero Trust Access Application, and a Service Token. It also writes the `cloudflared` credentials and config files locally.
2. **`bridge start --cloudflare`** launches `cloudflared tunnel run` as a managed child process. The tunnel routes traffic from your Cloudflare hostname to `localhost:8080`.
3. **iOS/Android apps** connect via `wss://agent.example.com`, sending `CF-Access-Client-Id` and `CF-Access-Client-Secret` headers obtained from the QR code. Cloudflare Access verifies these before forwarding traffic to the bridge.

## Prerequisites

- A Cloudflare account (free tier is sufficient)
- A domain managed by Cloudflare (e.g. `example.com`)
- A Cloudflare API token with the following permissions:
  - **Zone > DNS > Edit** (to create the CNAME record)
  - **Account > Cloudflare Tunnel > Edit** (to create and configure the tunnel)
  - **Account > Access: Apps and Policies > Edit** (to create the Access Application and Service Token)
- `cloudflared` installed on the machine running the bridge:
  ```bash
  # macOS
  brew install cloudflare/cloudflare/cloudflared

  # Linux (Debian/Ubuntu)
  curl -L --output cloudflared.deb https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64.deb
  sudo dpkg -i cloudflared.deb

  # Other platforms: https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/
  ```

## Step 1: One-Time Setup

Run `bridge setup` once to provision the Cloudflare infrastructure:

```bash
bridge setup \
  --api-token "your_cloudflare_api_token" \
  --account-id "your_cloudflare_account_id" \
  --domain "example.com" \
  --subdomain "agent" \
  --tunnel-name "my-bridge"
```

| Flag | Description |
|------|-------------|
| `--api-token` | Cloudflare API token (see Prerequisites above) |
| `--account-id` | Your Cloudflare account ID (found in the dashboard URL or Account Home) |
| `--domain` | Your Cloudflare-managed domain |
| `--subdomain` | Subdomain for the bridge (default: `agent`). Creates `agent.example.com` |
| `--tunnel-name` | Name for the Cloudflare Tunnel (default: `aptove-tunnel`) |

Setup creates:
- `~/.cloudflared/<tunnel-id>.json` — tunnel credentials for `cloudflared`
- `~/.cloudflared/config.yml` — `cloudflared` configuration (tunnel ID, ingress rules)
- `~/.config/bridge/config.json` — bridge config with all credentials and the Cloudflare hostname

At the end of setup, a QR code is printed containing the connection details (URL, Service Token credentials, auth token) ready to scan with the mobile app.

## Step 2: Start the Bridge

```bash
bridge start \
  --agent-command "copilot --acp" \
  --cloudflare \
  --qr
```

The `--cloudflare` flag tells the bridge to:
1. Verify `bridge setup` has been run (errors if not)
2. Check that `cloudflared` is on PATH (errors with install instructions if not)
3. Spawn `cloudflared tunnel run` using the config written during setup
4. Wait up to 30 seconds for the tunnel to become active
5. Print `🌐 Cloudflare tunnel active: https://agent.example.com`
6. Start the WebSocket server and accept connections

| Flag | Description |
|------|-------------|
| `--agent-command <CMD>` | Command to spawn the ACP agent (required) |
| `--cloudflare` | Enable managed Cloudflare tunnel mode |
| `--qr` | Display QR code for pairing (optional; useful on first run per device) |
| `--port <PORT>` | Local port (default: `8080`; must match port in setup's ingress rule) |
| `--keep-alive` | Keep agent processes alive when clients disconnect |
| `--session-timeout <SECS>` | Idle timeout before killing disconnected agents (default: 1800) |

## Step 3: Connect Mobile App

Scan the QR code printed during `bridge start --qr` (or the one printed at the end of `bridge setup`) with the Aptove iOS or Android app. The QR code contains:

- `url`: `https://agent.example.com` (the Cloudflare hostname)
- `clientId`: Cloudflare Service Token client ID
- `clientSecret`: Cloudflare Service Token client secret
- `authToken`: Bridge authentication token

The app automatically sends `CF-Access-Client-Id` and `CF-Access-Client-Secret` as HTTP headers on the WebSocket upgrade request whenever the URL scheme is `https://`.

## Checking Status

```bash
bridge status
```

Reports:
- Whether `config.json` exists and shows the configured hostname and tunnel ID
- Whether `~/.cloudflared/config.yml` exists

## Security Notes

- The Cloudflare Access Application enforces that only clients with a valid Service Token can reach the bridge. This is in addition to the bridge's own `X-Bridge-Token` authentication.
- Named tunnels (`~/.cloudflared/<tunnel-id>.json`) contain a secret. File permissions are set to `0600` automatically by `bridge setup`.
- The QR code contains the Service Token secret — treat it like a password. Re-generate the Service Token via `bridge setup` if compromised.

## Troubleshooting

| Symptom | Likely Cause | Fix |
|---------|-------------|-----|
| `Tunnel not configured. Run 'bridge setup' first.` | `bridge setup` was not run | Run `bridge setup` with valid API credentials |
| `cloudflared not found on PATH` | `cloudflared` not installed | Install per Prerequisites above |
| `cloudflared did not become ready within 30 seconds` | Tunnel misconfigured or network issue | Check `~/.cloudflared/config.yml`; run `cloudflared tunnel run --loglevel debug` manually |
| `403 Forbidden` from mobile app | Service Token rejected by Cloudflare Access | Re-run `bridge setup` to regenerate the Service Token; re-scan QR code |
| App connects but times out | Wrong port in ingress rule | Re-run `bridge setup` or edit `~/.cloudflared/config.yml` to match `--port` |

## Comparison with Local Transport

| Feature | Local (`--stdio-proxy`) | Cloudflare (`--cloudflare`) |
|---------|------------------------|----------------------------|
| Internet access | ❌ Same network only | ✅ Anywhere |
| Setup required | ✅ None | ⚠️ One-time `bridge setup` |
| Cloudflare account | Not needed | Required (free tier OK) |
| TLS certificate | Self-signed (pinned) | Cloudflare managed |
| Auth layers | Bridge token | Cloudflare Access + Bridge token |
| Latency | Minimal | Tunnel overhead (~10–50 ms) |
| Best for | Development, LAN | Production, remote use |

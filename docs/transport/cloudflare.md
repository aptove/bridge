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

1. **First run of `bridge start --cloudflare`** prompts for your Cloudflare API token, account ID, domain, and subdomain. It then provisions all required Cloudflare infrastructure automatically: a tunnel, a DNS CNAME record, a Zero Trust Access Application, and a Service Token. Credentials are saved to disk so this only happens once.
2. **Subsequent runs** load the saved config, check if the Service Token is nearing expiry (auto-rotating if within 30 days), then launch `cloudflared tunnel run` as a managed child process.
3. **A QR code is always shown** in Cloudflare mode. Scanning it triggers a secure one-time pairing handshake that delivers the connection URL, Service Token, and bridge auth token to the app in one step.
4. **iOS/Android apps** connect via `wss://agent.example.com`, sending `CF-Access-Client-Id` and `CF-Access-Client-Secret` headers. Cloudflare Access verifies these before forwarding traffic to the bridge.

## Prerequisites

- A Cloudflare account (free tier is sufficient)
- A domain managed by Cloudflare (e.g. `example.com`)
- A Cloudflare API token with the following permissions:
  - **Zone > DNS > Edit** (to create the CNAME record)
  - **Account > Cloudflare Tunnel > Edit** (to create and configure the tunnel)
  - **Account > Access: Apps and Policies > Edit** (to create the Access Application)
  - **Account > Access: Service Tokens > Edit** (to create and rotate Service Tokens)
- `cloudflared` installed on the machine running the bridge:
  ```bash
  # macOS
  brew install cloudflare/cloudflare/cloudflared

  # Linux (Debian/Ubuntu)
  curl -L --output cloudflared.deb https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64.deb
  sudo dpkg -i cloudflared.deb

  # Other platforms: https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/
  ```

## Starting the Bridge

There is no separate setup command. Simply run:

```bash
bridge start \
  --agent-command "copilot --acp" \
  --cloudflare
```

### First Run

On first run the bridge prompts interactively:

```
🔧 Cloudflare Zero Trust is not configured yet. Let's set it up now.
   (You only need to do this once — credentials are saved to disk.)

  Cloudflare API Token (Zones:Edit + Access:*:Edit + Service Tokens:Edit): ••••••
  Cloudflare Account ID: abc123
  Domain (e.g. example.com): example.com
  Subdomain [agent]:
```

It then automatically:
1. Creates (or reuses) a named Cloudflare Tunnel
2. Creates (or updates) the DNS CNAME record for your subdomain
3. Creates the Zero Trust Access Application with a Service Token policy
4. Issues a Service Token (clientId + clientSecret)
5. Configures tunnel ingress rules
6. Writes `~/.cloudflared/<tunnel-id>.json` and `~/.cloudflared/config.yml`
7. Saves all credentials to `~/.config/bridge/bridge/config.json` (permissions `0600`)
8. Launches `cloudflared` and waits for the tunnel to become active
9. Prints a QR code — scan it once with the mobile app

### Subsequent Runs

On subsequent runs the bridge loads the saved config, skips all prompts, checks the Service Token expiry, and starts immediately:

```bash
bridge start --agent-command "copilot --acp" --cloudflare
```

The QR code is shown on every start. Each session's QR encodes a fresh one-time pairing code (valid for 60 seconds) at:
```
https://agent.example.com/pair/local?code=438291
```
The app exchanges this code for the full credentials (URL, clientId, clientSecret, authToken) via a single HTTPS request. After the first scan, the app stores everything in the iOS Keychain and reconnects automatically on future sessions — no re-scan required unless credentials change.

| Flag | Description |
|------|-------------|
| `--agent-command <CMD>` | Command to spawn the ACP agent (required) |
| `--cloudflare` | Enable managed Cloudflare tunnel mode |
| `--port <PORT>` | Local port (default: `8080`) |
| `--keep-alive` | Keep agent processes alive when clients disconnect |
| `--session-timeout <SECS>` | Idle timeout before killing disconnected agents (default: 1800) |

## Service Token Auto-Rotation

Service Tokens are issued with a 1-year lifetime. The bridge tracks the issuance date in config. When fewer than 30 days remain (or no issuance date is recorded):

1. The bridge automatically issues a new Service Token via the Cloudflare API
2. Saves the new `clientId`/`clientSecret` to config
3. Prints the QR code with the updated credentials

**The user must re-scan the QR code once after rotation** so the app picks up the new credentials. The bridge logs a clear message when this happens:

```
🔄 Cloudflare service token is expiring — auto-rotating...
✅ Service token rotated — re-scan QR code on your mobile app
```

The API token is stored in `config.json` (alongside the other secrets) to enable silent rotation without re-prompting.

## Credential Layers

Two independent auth layers protect every connection:

| Layer | Credential | Lifetime | Checked by |
|-------|-----------|---------|------------|
| Cloudflare Access | `CF-Access-Client-Id` + `CF-Access-Client-Secret` | 1 year (auto-rotated) | Cloudflare Edge |
| Bridge auth | `X-Bridge-Token` | Permanent (until config deleted) | Bridge WebSocket handler |

The app must present both. Cloudflare blocks unauthenticated requests before they reach the bridge.

## Credentials Stored on the Mobile App

After scanning the QR code the app stores in the iOS Keychain (device-locked, non-exportable):

| Field | Purpose | Mobile app needs it? |
|-------|---------|----------------------|
| `url` | WebSocket endpoint, e.g. `wss://agent.example.com` | ✅ |
| `clientId` | `CF-Access-Client-Id` header value | ✅ |
| `clientSecret` | `CF-Access-Client-Secret` header value | ✅ |
| `authToken` | `X-Bridge-Token` header value | ✅ |
| `TunnelSecret` | Authenticates `cloudflared` to Cloudflare's edge | ❌ server-side only |

**TunnelSecret** is a random 32-byte value generated by the bridge when the tunnel is first created. It is stored in `~/.cloudflared/<tunnel-id>.json` and used exclusively by the `cloudflared` process to prove to Cloudflare's edge that it owns the tunnel. It never leaves the machine and is never sent to the mobile app or included in the QR code.

## Checking Status

```bash
bridge status
```

Reports whether `config.json` exists and shows the configured hostname and tunnel ID.

## Security Notes

- `config.json` contains the Cloudflare API token, Service Token secret, and bridge auth token. File permissions are set to `0600` automatically.
- The QR pairing code is valid for 60 seconds and single-use. The underlying secrets are never encoded directly in the QR image.
- The Service Token secret (`clientSecret`) is only available at issuance time and is never retrievable from the Cloudflare API after that. If lost, the bridge deletes the old token and issues a fresh one automatically.

## Troubleshooting

| Symptom | Likely Cause | Fix |
|---------|-------------|-----|
| `cloudflared not found on PATH` | `cloudflared` not installed | Install per Prerequisites above |
| `cloudflared did not become ready within 30 seconds` | Tunnel misconfigured or network issue | Check `~/.cloudflared/config.yml`; run `cloudflared tunnel run --loglevel debug` manually |
| `Authentication error (code 10000)` during setup | API token missing `Access: Service Tokens: Edit` permission | Edit the token in Cloudflare dashboard and add that permission |
| App gets "bad response from server" | Bridge not running, or Service Token expired | Ensure `bridge start --cloudflare` is running; re-scan QR if token was rotated |
| App connects but times out | Wrong port in ingress rule | Delete config and re-run `bridge start --cloudflare` to re-provision |
| Want to force re-setup | Any reason | `rm ~/.config/bridge/bridge/config.json` then re-run `bridge start --cloudflare` |

## Comparison with Other Transports

| Feature | Local (`--stdio-proxy`) | Cloudflare (`--cloudflare`) | Tailscale (`--tailscale`) |
|---------|------------------------|----------------------------|--------------------------|
| Internet access | ❌ Same network only | ✅ Anywhere | ✅ Tailnet / public (funnel) |
| First-run setup | ✅ None | ⚠️ Interactive prompt (once) | ⚠️ Tailscale must be installed |
| External account | Not needed | Cloudflare (free OK) | Tailscale (free OK) |
| TLS certificate | Self-signed (pinned) | Cloudflare managed | Tailscale managed (serve) / self-signed (ip) |
| Auth layers | Bridge token | Cloudflare Access + Bridge token | Bridge token |
| Latency | Minimal | Tunnel overhead (~10–50 ms) | Minimal (direct Tailnet) |
| Best for | Development, LAN | Production, remote use | Team / personal device mesh |

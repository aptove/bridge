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
                                          │  localhost:<port>   │ stdio │  (your cmd)  │
                                          └─────────────────────┘       └──────────────┘
```

---

## Prerequisites

- A Cloudflare account (free tier is sufficient)
- A domain managed by Cloudflare (e.g. `example.com`)
- A Cloudflare API token with the following permissions:
  - **Zone → DNS → Edit** (to create the CNAME record)
  - **Account → Cloudflare Tunnel → Edit** (to create and configure the tunnel)
  - **Account → Access: Apps and Policies → Edit** (to create the Access Application)
  - **Account → Access: Service Tokens → Edit** (to create and rotate Service Tokens)
- `cloudflared` installed on the machine running the bridge:
  ```bash
  # macOS
  brew install cloudflare/cloudflare/cloudflared

  # Linux (Debian/Ubuntu)
  curl -L --output cloudflared.deb https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64.deb
  sudo dpkg -i cloudflared.deb

  # Other platforms: https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/
  ```

---

## One-Time Setup

Run the `setup` command once to provision Cloudflare infrastructure and write credentials to `common.toml`:

```bash
bridge setup \
  --api-token  "your_api_token" \
  --account-id "your_account_id" \
  --domain     "example.com" \
  --subdomain  "agent"
```

| Flag | Description | Default |
|------|-------------|---------|
| `--api-token <TOKEN>` | Cloudflare API token | Required |
| `--account-id <ID>` | Cloudflare account ID | Required |
| `--domain <DOMAIN>` | Domain managed by Cloudflare | Required |
| `--subdomain <SUB>` | Subdomain for the bridge endpoint | `agent` |
| `--tunnel-name <NAME>` | Name for the Cloudflare tunnel | `aptove-tunnel` |

`setup` automatically:
1. Creates (or reuses) a named Cloudflare Tunnel
2. Creates (or updates) the DNS CNAME record for your subdomain
3. Creates the Zero Trust Access Application with a Service Token policy
4. Issues a Service Token (clientId + clientSecret)
5. Configures tunnel ingress rules
6. Writes `~/.cloudflared/<tunnel-id>.json` and `~/.cloudflared/config.yml`
7. Saves all credentials to `common.toml` under `[transports.cloudflare]` with `enabled = true`

After setup, verify `common.toml` contains:

```toml
[transports.cloudflare]
enabled       = true
hostname      = "https://agent.example.com"
tunnel_id     = "abc123"
tunnel_secret = "..."
account_id    = "..."
client_id     = "client.access"
client_secret = "xxxxx"
```

---

## Starting the Bridge

```bash
bridge start --agent-command "gemini --experimental-acp" --qr
```

Transport selection is read from `common.toml` — no `--cloudflare` flag is needed. The bridge:

1. Loads Cloudflare credentials from `common.toml`
2. Checks Service Token expiry and auto-rotates if within 30 days
3. Launches `cloudflared tunnel run` as a managed child process
4. Waits up to 30 seconds for the tunnel to become active
5. Shows a QR code (if `--qr` is passed)

---

## Service Token Auto-Rotation

Service Tokens are issued with a 1-year lifetime. The bridge tracks the issuance date in `common.toml`. When fewer than 30 days remain:

1. The bridge automatically issues a new Service Token via the Cloudflare API
2. Saves the new `clientId`/`clientSecret` to `common.toml`
3. Shows the QR code with updated credentials

**The user must re-scan the QR code once after rotation** so the app picks up the new credentials. The bridge logs:

```
🔄 Cloudflare service token is expiring — auto-rotating...
✅ Service token rotated — re-scan QR code on your mobile app
```

---

## Credential Layers

Two independent auth layers protect every connection:

| Layer | Credential | Lifetime | Checked by |
|-------|-----------|---------|------------|
| Cloudflare Access | `CF-Access-Client-Id` + `CF-Access-Client-Secret` | 1 year (auto-rotated) | Cloudflare Edge |
| Bridge auth | `X-Bridge-Token` | Permanent (until `common.toml` deleted) | Bridge WebSocket handler |

The app must present both. Cloudflare blocks unauthenticated requests before they reach the bridge.

---

## Credentials Stored on the Mobile App

After scanning the QR code, the app stores in the iOS Keychain / Android EncryptedSharedPreferences:

| Field | Purpose |
|-------|---------|
| `agentId` | Stable UUID for multi-transport dedup |
| `url` | WebSocket endpoint, e.g. `wss://agent.example.com` |
| `clientId` | `CF-Access-Client-Id` header value |
| `clientSecret` | `CF-Access-Client-Secret` header value |
| `authToken` | `X-Bridge-Token` header value |

`tunnel_secret` (in `common.toml`) is used exclusively by `cloudflared` on the server side and is never sent to the mobile app.

---

## Checking Status

```bash
bridge status
```

Prints `agent_id`, `common.toml` path, whether `[transports.cloudflare]` is enabled, and whether `~/.cloudflared/config.yml` exists.

---

## Forcing Re-Setup

To reprovision all Cloudflare infrastructure from scratch:

```bash
# Remove cloudflare section from common.toml (or delete the file entirely)
rm ~/Library/Application\ Support/com.aptove.bridge/common.toml   # macOS
rm ~/.config/bridge/common.toml                                    # Linux

# Re-run setup
bridge setup --api-token "..." --account-id "..." --domain "example.com"
```

---

## Security Notes

- `common.toml` contains the Cloudflare API token, Service Token secret, and bridge auth token. File permissions are set to `0600` automatically.
- The QR pairing code is valid for 60 seconds and single-use. The underlying secrets are never encoded directly in the QR image.
- The Service Token secret (`clientSecret`) is only available at issuance time and is never retrievable from the Cloudflare API afterwards. If lost, the bridge deletes the old token and issues a fresh one automatically on the next run.

---

## Troubleshooting

| Symptom | Likely Cause | Fix |
|---------|-------------|-----|
| `cloudflared not found on PATH` | `cloudflared` not installed | Install per Prerequisites above |
| `cloudflared did not become ready within 30 seconds` | Tunnel misconfigured or network issue | Check `~/.cloudflared/config.yml`; run `cloudflared tunnel run --loglevel debug` manually |
| `Authentication error (code 10000)` during setup | API token missing `Access: Service Tokens: Edit` permission | Edit the token in Cloudflare dashboard and add that permission |
| App gets "bad response from server" | Bridge not running or Service Token expired | Ensure `bridge start` is running; re-scan QR if token was rotated |
| App connects but times out | Wrong port in ingress rule | Delete `common.toml` and re-run `bridge setup` |
| "403 Forbidden" from mobile | Missing `CF-Access-Client-Id`/`CF-Access-Client-Secret` headers | Re-scan the QR code |

---

## Comparison with Other Transports

| Feature | Local | Cloudflare | Tailscale |
|---------|-------|------------|-----------|
| Internet access | ❌ Same network only | ✅ Anywhere | ✅ Tailnet |
| First-run setup | ✅ None | ⚠️ `bridge setup` (once) | ⚠️ Tailscale must be installed |
| External account | Not needed | Cloudflare (free OK) | Tailscale (free OK) |
| TLS certificate | Self-signed (pinned) | Cloudflare managed | Tailscale managed (serve) / self-signed (ip) |
| Auth layers | Bridge token | Cloudflare Access + Bridge token | Bridge token |
| Latency | Minimal | Tunnel overhead (~10–50 ms) | Minimal (direct Tailnet) |
| Best for | Development, LAN | Production, remote | Team / personal device mesh |

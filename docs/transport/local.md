# Local Transport

The local transport enables secure connections between client apps (iOS, Android, desktop) and the bridge running on the same local network. This is ideal for development, local-first deployments, and scenarios where Cloudflare tunnels or Tailscale aren't needed.

## Overview

```
┌─────────────────┐      Local Network       ┌─────────────────┐
│   Mobile App    │◄────────TLS/WSS─────────►│     Bridge      │
│  (iOS/Android)  │                          │  (Your Machine) │
└─────────────────┘                          └─────────────────┘
        │                                            │
        │  1. Scan QR code                           │
        │  2. Validate TLS fingerprint               │
        │  3. GET /pair/local?code=XXXXXX            │
        │  4. Receive credentials                    │
        │  5. Connect via WebSocket                  │
        ▼                                            ▼
```

---

## Configuration

Local transport is enabled by default. To customise, edit `common.toml`:

```toml
[transports.local]
enabled = true
port    = 8765    # default: 8765
tls     = true    # default: true
```

**Config file location:**
- macOS: `~/Library/Application Support/com.aptove.bridge/common.toml`
- Linux: `~/.config/bridge/common.toml`

---

## Starting the Bridge

```bash
bridge run --agent-command "aptove" --qr
```

The bridge reads port, TLS, and auth token settings from `common.toml` — no flags needed for those.

**Available `run` flags:**

| Flag | Description | Default |
|------|-------------|---------|
| `--agent-command <CMD>` | Command to spawn the ACP agent | Required |
| `--bind <ADDR>` | Bind address for the WebSocket server | `0.0.0.0` |
| `--advertise-addr <IP>` | IP or hostname advertised in the QR code and embedded in the TLS cert SANs. Required when the bridge's local IP differs from the address reachable by clients (e.g. inside a container). | Auto-detected |
| `--qr` | Display QR code for mobile pairing | Off |
| `--verbose` | Enable info-level logging | Off |

---

## Container Usage

When running the bridge inside a Docker or Apple Native container, the bridge's
local IP is the container's internal virtual IP — not your machine's LAN IP.
Use `--advertise-addr` to override both the QR code URL **and** the TLS
certificate's Subject Alternative Names (SANs) so mobile clients can connect:

```bash
# Docker
docker run -p 8765:8765 \
  -v "$HOME/Library/Application Support/com.aptove.bridge":/root/.config/bridge \
  aptove/bridge \
  run --agent-command "aptove" --advertise-addr 192.168.1.50 --qr

# Apple Native container (macOS)
container run -p 8765:8765 \
  -v "$HOME/Library/Application Support/com.aptove.bridge":/root/.config/bridge \
  aptove/bridge \
  run --agent-command "aptove" --advertise-addr 192.168.1.50 --qr
```

> **Important:** If you add `--advertise-addr` for the first time (or change the
> IP), the existing TLS certificate is regenerated because the SANs changed.
> You will need to delete the old cert files and re-pair the mobile app:
>
> ```bash
> rm ~/Library/Application\ Support/com.aptove.bridge/cert.pem
> rm ~/Library/Application\ Support/com.aptove.bridge/key.pem
> ```

---

## Pairing Flow

### 1. QR Code Display

When started with `--qr`, the bridge displays:

```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  ⏱️  QR code expires in 59 seconds | Single use only
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  [QR CODE]

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  📱 Scan QR code with your mobile app
  🔗 https://192.168.1.100:8765/pair/local?code=847291&fp=SHA256%3A...
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

### 2. QR Code Content

The QR code encodes a pairing URL:

```
https://<IP>:<PORT>/pair/local?code=<PAIRING_CODE>&fp=<TLS_FINGERPRINT>
```

| Parameter | Description |
|-----------|-------------|
| `code` | 6-digit one-time pairing code (expires in 60 seconds) |
| `fp` | SHA256 fingerprint of the TLS certificate (URL-encoded) |

### 3. Pairing Endpoint

**Request:**
```
GET /pair/local?code=847291
Host: 192.168.1.100:8765
```

**Success Response (200 OK):**
```json
{
  "agentId":        "550e8400-e29b-41d4-a716-446655440000",
  "url":            "wss://192.168.1.100:8765",
  "protocol":       "acp",
  "version":        "1.0",
  "authToken":      "base64urltoken",
  "certFingerprint":"SHA256:ABCD1234..."
}
```

**Error Responses:**

| Status | Error | Description |
|--------|-------|-------------|
| 401 | `invalid_code` | Code is wrong, expired, or already used |
| 429 | `rate_limited` | Too many failed attempts (5 max) |

`agentId` is a stable UUID that lets the mobile app recognise the same agent across multiple transports — scanning a second transport's QR adds a new endpoint instead of creating a duplicate agent entry.

### 4. WebSocket Connection

After pairing, connect to the WebSocket URL with the auth token:

```
GET / HTTP/1.1
Host: 192.168.1.100:8765
Upgrade: websocket
Connection: Upgrade
X-Bridge-Token: <authToken>
```

Or via query parameter:
```
wss://192.168.1.100:8765?token=<authToken>
```

---

## Offline Registration (`show-qr` without the bridge running)

You can pre-register a mobile device before starting the full bridge:

```bash
bridge show-qr
```

If the bridge is not running, this starts a lightweight pairing-only server, shows a QR code, waits for the mobile app to complete the handshake, then exits. The bridge doesn't need to be running to complete pairing.

---

## Security Design

### Pairing Code Security

| Property | Value | Purpose |
|----------|-------|---------|
| Length | 6 digits | Easy to type manually if needed |
| Expiry | 60 seconds | Limits exposure window |
| Usage | Single-use | Prevents replay attacks |
| Attempts | 5 max | Prevents brute-force |

### TLS Certificate

The bridge generates a self-signed TLS certificate on first run and saves it as
`cert.pem` / `key.pem` in the config directory. The certificate includes the
following Subject Alternative Names (SANs):

- `localhost` and `127.0.0.1` (always)
- The machine's detected local network IP (always, when available)
- The `--advertise-addr` value (when provided)

The certificate fingerprint is included in the QR pairing URL and must be
validated by the mobile app before trusting the connection. The cert is reused
across restarts; it is only regenerated when the SANs change (e.g. a new
`--advertise-addr` is provided) or when `cert.pem` / `key.pem` are deleted.

### Credentials and Auth Token

`auth_token` is auto-generated (32 bytes, URL-safe base64) and stored in
`common.toml` with `0600` permissions. It persists across restarts — paired
devices reconnect without re-scanning.

### Rotating Credentials

**Rotate TLS certificate only** (invalidates all paired devices — they must re-scan):

```bash
# macOS
rm ~/Library/Application\ Support/com.aptove.bridge/cert.pem \
   ~/Library/Application\ Support/com.aptove.bridge/key.pem

# Linux
rm ~/.config/bridge/cert.pem ~/.config/bridge/key.pem
```

**Full reset** (regenerates cert, auth token, and agent ID):

```bash
# macOS
rm ~/Library/Application\ Support/com.aptove.bridge/cert.pem \
   ~/Library/Application\ Support/com.aptove.bridge/key.pem \
   ~/Library/Application\ Support/com.aptove.bridge/common.toml

# Linux
rm ~/.config/bridge/cert.pem ~/.config/bridge/key.pem ~/.config/bridge/common.toml
```

Then re-run `bridge run --agent-command "..." --qr`.

---

## Manual Testing with curl

```bash
# Note: -k disables cert verification (for testing only)
curl -k "https://192.168.1.100:8765/pair/local?code=847291"
```

---

## Troubleshooting

### "Connection refused"
- Ensure the bridge is running
- Check firewall settings allow the configured port (default `8765`)
- Verify the bridge is binding to the right interface (`--bind 0.0.0.0` by default)

### "Invalid code"
- Codes expire after 60 seconds — restart `bridge run --qr` to get a fresh code
- Codes are single-use — scan only once
- Check for typos if entering the code manually

### "Rate limited"
- Too many failed pairing attempts on the current code
- Restart the bridge to issue a fresh code

### TLS / certificate errors
- Mobile apps validate the fingerprint from the QR code `fp` parameter — ensure the app scanned the most recent QR
- If running in a container, use `--advertise-addr <LAN_IP>` so the cert includes the correct IP in its SANs; then delete `cert.pem` / `key.pem` and re-pair
- If you deleted `cert.pem` / `key.pem` and restarted, the fingerprint changed — re-pair all devices

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                         Bridge                              │
│  ┌─────────────┐  ┌──────────────┐  ┌───────────────────┐   │
│  │   TLS       │  │   Pairing    │  │    WebSocket      │   │
│  │   Server    │──│   Manager    │──│    Handler        │   │
│  │             │  │              │  │                   │   │
│  │ Self-signed │  │ - Code gen   │  │ - Auth validation │   │
│  │ certificate │  │ - Validation │  │ - Message routing │   │
│  │             │  │ - Rate limit │  │ - Agent stdio     │   │
│  └─────────────┘  └──────────────┘  └───────────────────┘   │
│         │                │                    │             │
│         └────────────────┴────────────────────┘             │
│                          │                                  │
│                    Port 8765 (default)                      │
└─────────────────────────────────────────────────────────────┘
                           │
              ┌────────────┼────────────┐
              │            │            │
         /pair/local    WebSocket    Other
         (HTTP GET)     Upgrade      Requests
```

## Comparison with Other Transports

| Feature | Local | Cloudflare | Tailscale |
|---------|-------|------------|-----------|
| Internet access | ❌ Same network only | ✅ Anywhere | ✅ Tailnet |
| Setup complexity | ✅ None | ⚠️ One-time account setup | ⚠️ Tailscale required |
| Latency | ✅ Minimal | ⚠️ Tunnel overhead | ✅ Minimal (direct) |
| TLS certificate | Self-signed (pinned) | Cloudflare managed | Tailscale managed (serve) / self-signed (ip) |
| Best for | Development, LAN | Production, remote | Team / personal mesh |

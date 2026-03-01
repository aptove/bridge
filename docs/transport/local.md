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
bridge run --agent-command "gemini --experimental-acp" --qr
```

The bridge reads port, TLS, and auth token settings from `common.toml` — no flags needed.

**Available `start` flags:**

| Flag | Description | Default |
|------|-------------|---------|
| `--agent-command <CMD>` | Command to spawn the ACP agent | Required |
| `--bind <ADDR>` | Bind address | `0.0.0.0` |
| `--qr` | Display QR code for mobile pairing | Off |
| `--verbose` | Enable info-level logging | Off |

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

### TLS Certificate Pinning

The bridge generates a self-signed TLS certificate on first run. The certificate fingerprint is included in the QR pairing URL and must be validated by the mobile app before trusting the connection.

### Credentials and Auth Token

`auth_token` is auto-generated (32 bytes, URL-safe base64) and stored in `common.toml` with `0600` permissions. It persists across restarts — paired devices reconnect without re-scanning.

### Rotating Credentials

To regenerate `auth_token`, `agent_id`, and TLS cert (invalidates all paired devices):

```bash
rm ~/Library/Application\ Support/com.aptove.bridge/common.toml   # macOS
rm ~/.config/bridge/common.toml                                    # Linux
bridge run --agent-command "..." --qr
```

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
- Run `bridge status` to confirm the port and IP

### "Invalid code"
- Codes expire after 60 seconds — re-run `bridge run --qr` to get a fresh code
- Codes are single-use — scan only once
- Check for typos if entering the code manually

### "Rate limited"
- Too many failed pairing attempts on the current code
- Restart the bridge to issue a fresh code

### Certificate errors
- Mobile apps must validate the fingerprint from the QR code `fp` parameter
- The fingerprint is per-certificate — if you delete `common.toml` and restart, the cert changes and you must re-pair

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

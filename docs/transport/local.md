# Local Transport

The local transport enables secure connections between client apps (iOS, Android, desktop) and the bridge running on the same local network. This is ideal for development, local-first deployments, and scenarios where Cloudflare tunnels aren't needed.

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

## Starting the Bridge

```bash
bridge start --agent-command "your-agent-command" --stdio-proxy --qr
```

### Options

| Flag | Description |
|------|-------------|
| `--stdio-proxy` | Run in local mode (no Cloudflare) |
| `--qr` | Display QR code for mobile pairing |
| `--port <PORT>` | WebSocket port (default: 8080) |
| `--bind <ADDR>` | Bind address (default: 0.0.0.0) |
| `--no-auth` | Disable authentication (NOT recommended) |
| `--no-tls` | Disable TLS (NOT recommended) |

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
  🔗 https://192.168.1.100:8080/pair/local?code=847291&fp=SHA256%3A...
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

### 2. QR Code Content

The QR code encodes a pairing URL:

```
https://<IP>:<PORT>/pair/local?code=<PAIRING_CODE>&fp=<TLS_FINGERPRINT>
```

| Parameter | Description |
|-----------|-------------|
| `code` | 6-digit one-time pairing code |
| `fp` | SHA256 fingerprint of the TLS certificate (URL-encoded) |

### 3. Pairing Endpoint

**Request:**
```
GET /pair/local?code=847291&fp=SHA256:ABCD...
Host: 192.168.1.100:8080
```

**Success Response (200 OK):**
```json
{
  "url": "wss://192.168.1.100:8080",
  "protocol": "acp",
  "version": "1.0",
  "authToken": "base64-encoded-token",
  "certFingerprint": "SHA256:ABCD1234..."
}
```

**Error Responses:**

| Status | Error | Description |
|--------|-------|-------------|
| 401 | `invalid_code` | Code is wrong, expired, or already used |
| 429 | `rate_limited` | Too many failed attempts (5 max) |

### 4. WebSocket Connection

After successful pairing, connect to the WebSocket URL with the auth token:

```
GET / HTTP/1.1
Host: 192.168.1.100:8080
Upgrade: websocket
Connection: Upgrade
X-Bridge-Token: <authToken>
```

Or via query parameter:
```
wss://192.168.1.100:8080?token=<authToken>
```

## Security Design

### Pairing Code Security

| Property | Value | Purpose |
|----------|-------|---------|
| Length | 6 digits | Easy to type if needed |
| Expiry | 60 seconds | Limits exposure window |
| Usage | One-time | Prevents replay attacks |
| Attempts | 5 max | Prevents brute-force |

**Brute-force analysis:**
- 6 digits = 1,000,000 combinations
- 5 attempts in 60 seconds = negligible success probability
- After 5 failures, code is invalidated

### TLS Certificate Pinning

The bridge uses a self-signed TLS certificate. To prevent MITM attacks:

1. **Certificate fingerprint is embedded in the QR code URL**
2. **Client apps MUST validate the fingerprint** before trusting the connection

**Client implementation (pseudo-code):**
```swift
// iOS/Swift example
func validateCertificate(serverCert: SecCertificate, expectedFingerprint: String) -> Bool {
    let serverFingerprint = sha256Fingerprint(of: serverCert)
    return serverFingerprint == expectedFingerprint
}
```

### Why Self-Signed?

- **Let's Encrypt requires public domain validation** - not possible for local IPs
- **Certificate pinning is MORE secure** than CA validation when you know the expected cert
- **No external dependencies** - works offline and in air-gapped environments

## Client Implementation Guide

### Mobile Apps (QR Scan Flow)

1. **Scan QR code** → Extract URL
2. **Parse URL** → Get `code` and `fp` parameters
3. **Create HTTPS request** with custom certificate validation:
   - Extract server certificate fingerprint
   - Compare with `fp` from URL
   - Reject if mismatch (potential MITM)
4. **Call pairing endpoint** → Receive credentials
5. **Store credentials** securely
6. **Connect WebSocket** with auth token

### Desktop Apps

Since desktop apps can't scan QR codes from their own screen:

1. **Read the URL** from terminal output
2. **Make HTTPS request** with certificate pinning
3. **Connect WebSocket** with received credentials

### Manual Testing with curl

```bash
# Note: -k disables cert verification (for testing only)
curl -k "https://192.168.1.100:8080/pair/local?code=847291"
```

## Troubleshooting

### "Connection refused"
- Ensure bridge is running
- Check firewall settings allow port 8080
- Verify IP address is correct

### "Invalid code"
- Code expires after 60 seconds - restart bridge for new code
- Code can only be used once
- Check for typos if entering manually

### "Rate limited"
- Too many failed attempts
- Restart bridge to get a fresh code

### Certificate errors
- Mobile apps must implement certificate pinning
- Don't blindly trust all self-signed certs
- Compare fingerprint from QR with server's actual cert

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                         Bridge                               │
│  ┌─────────────┐  ┌──────────────┐  ┌───────────────────┐   │
│  │   TLS       │  │   Pairing    │  │    WebSocket      │   │
│  │   Server    │──│   Manager    │──│    Handler        │   │
│  │             │  │              │  │                   │   │
│  │ Self-signed │  │ - Code gen   │  │ - Auth validation │   │
│  │ certificate │  │ - Validation │  │ - Message routing │   │
│  │             │  │ - Rate limit │  │ - Agent stdio     │   │
│  └─────────────┘  └──────────────┘  └───────────────────┘   │
│         │                │                    │              │
│         └────────────────┴────────────────────┘              │
│                          │                                   │
│                    Port 8080                                 │
└─────────────────────────────────────────────────────────────┘
                           │
              ┌────────────┼────────────┐
              │            │            │
         /pair/local    WebSocket    Other
         (HTTP GET)     Upgrade      Requests
```

## Configuration

### Environment Variables

None required for local mode.

### Persistent Data

- **TLS Certificate**: `~/.config/bridge/cert.pem`
- **TLS Private Key**: `~/.config/bridge/key.pem`
- **Auth Token**: Generated per session (not persisted in local mode)

## Comparison with Cloudflare Transport

| Feature | Local | Cloudflare |
|---------|-------|------------|
| Internet access | ❌ Same network only | ✅ Anywhere |
| Setup complexity | ✅ None | ⚠️ Requires account |
| Latency | ✅ Minimal | ⚠️ Tunnel overhead |
| TLS certificate | Self-signed | Cloudflare managed |
| Use case | Development, local | Production, remote |

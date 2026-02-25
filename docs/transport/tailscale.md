# Tailscale Transport

Use Tailscale as the transport layer for the ACP bridge. This lets you connect from any device on your tailnet without exposing a port to the public internet.

Two modes are available and can run simultaneously:

| Mode | How it works | TLS | Best for |
|------|-------------|-----|---------|
| `tailscale-serve` | `tailscale serve` proxies HTTPS 443 → bridge localhost port | Tailscale-managed (trusted CA) | Recommended — no cert pinning needed |
| `tailscale-ip` | Bridge binds directly to Tailscale IP (`100.x.x.x`) with self-signed TLS | Self-signed (fingerprint-pinned) | Fallback when MagicDNS/HTTPS not enabled |

---

## Prerequisites

- **Tailscale v1.38+** installed on the machine running the bridge
  - macOS: `brew install tailscale`
  - Linux: follow [tailscale.com/download](https://tailscale.com/download)
- Machine enrolled in a tailnet: `tailscale up`
- **Mobile devices must be on the same tailnet** — install the Tailscale app on iOS/Android and sign in to the same account before pairing

For `tailscale-serve` only:
- **MagicDNS** enabled in the Tailscale admin console
- **HTTPS certificates** enabled — see [Enabling HTTPS](https://tailscale.com/kb/1153/enabling-https)

---

## Configuration

Enable Tailscale transport(s) in `common.toml`:

### `tailscale-serve` mode

```toml
[transports.tailscale-serve]
enabled = true
```

No port or TLS fields needed — Tailscale handles HTTPS on port 443 and routes to an auto-selected localhost port.

### `tailscale-ip` mode

```toml
[transports.tailscale-ip]
enabled = true
port    = 8765   # optional, defaults to 8765
tls     = true   # optional, defaults to true
```

### Both modes simultaneously

```toml
[transports.tailscale-serve]
enabled = true

[transports.tailscale-ip]
enabled = true
port    = 8765
```

---

## Starting the Bridge

```bash
bridge start --agent-command "gemini --experimental-acp" --qr
```

That's all. Transport mode is read from `common.toml` — no extra flags needed.

---

## What Happens at Startup

### `tailscale-serve` mode

1. The bridge detects your MagicDNS hostname (e.g. `my-laptop.tail1234.ts.net`).
2. `tailscale serve --https=443 http://localhost:<port>` is configured automatically.
3. The pairing URL uses `wss://my-laptop.tail1234.ts.net` — no certificate fingerprint needed because Tailscale provides a CA-signed certificate.
4. When the bridge exits, `tailscale serve reset` cleans up the serve configuration automatically.

### `tailscale-ip` mode

1. The bridge detects the Tailscale IPv4 address (`100.x.x.x`) and MagicDNS hostname (if available).
2. A self-signed TLS certificate is generated with the Tailscale IP (and hostname) as Subject Alternative Names.
3. The pairing URL uses `wss://100.x.x.x:8765` with a certificate fingerprint for pinning.

---

## Pairing URL Formats

### `tailscale-serve` (no fingerprint — valid CA cert)

```
wss://my-laptop.tail1234.ts.net
```

### `tailscale-ip` (with fingerprint)

```
wss://100.x.x.x:8765
```

The QR encodes a pairing endpoint. After scanning, the mobile app calls:

```
GET https://my-laptop.tail1234.ts.net/pair/tailscale?code=847291
```

and receives credentials:

```json
{
  "agentId":        "550e8400-e29b-41d4-a716-446655440000",
  "url":            "wss://my-laptop.tail1234.ts.net",
  "protocol":       "acp",
  "version":        "1.0",
  "authToken":      "base64urltoken"
}
```

(`certFingerprint` is included for `tailscale-ip` mode but omitted for `tailscale-serve`.)

---

## Mobile Client Notes

- The mobile device must be connected to the same tailnet before pairing. Verify with the Tailscale app that the device can reach the bridge host.
- After pairing, the app reconnects automatically as long as the device remains on the tailnet.
- If `tailscale-ip` mode is used and the Tailscale IP changes (e.g. you join a different tailnet), the TLS certificate is regenerated on the next bridge start and the mobile app must re-pair.

---

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `tailscale: not found on PATH` | Tailscale not installed | Install from [tailscale.com/download](https://tailscale.com/download) |
| `Not enrolled in a Tailscale network` | Machine not connected | Run `tailscale up` |
| `tailscale-serve mode requires MagicDNS + HTTPS` | MagicDNS or HTTPS not enabled on tailnet | Enable in [Tailscale admin console](https://login.tailscale.com/admin/dns) → DNS |
| `tailscale serve requires Tailscale v1.38+` | Outdated Tailscale | Update Tailscale |
| Certificate changed warning on start | Tailscale IP changed since last cert generation | Expected — cert is regenerated; re-scan QR code on the mobile app |
| App cannot reach bridge after pairing | Mobile not on the same tailnet | Open Tailscale app on the phone, ensure it's signed in and connected |
| `bridge show-qr` shows wrong IP | Local IP detection fallback | Check `bridge status` for the detected Tailscale IP |

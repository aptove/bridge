# Tailscale Transport

Use Tailscale as the transport layer for the ACP bridge. This lets you connect from any device on your tailnet without exposing a port to the public internet.

The `tailscale-serve` mode uses `tailscale serve` to proxy HTTPS on port 443 to the bridge's localhost port. TLS is managed by Tailscale using a CA-signed certificate — no certificate pinning required.

---

## Prerequisites

- **Tailscale v1.38+** installed on the machine running the bridge
  - macOS: `brew install tailscale`
  - Linux: follow [tailscale.com/download](https://tailscale.com/download)
- Machine enrolled in a tailnet: `tailscale up`
- **MagicDNS** enabled in the Tailscale admin console
- **HTTPS certificates** enabled — see [Enabling HTTPS](https://tailscale.com/kb/1153/enabling-https)
- **Mobile devices must be on the same tailnet** — install the Tailscale app on iOS/Android and sign in to the same account before pairing

---

## Configuration

Enable Tailscale in `common.toml`:

```toml
[transports.tailscale-serve]
enabled = true
```

No port or TLS fields needed — Tailscale handles HTTPS on port 443 and routes to an auto-selected localhost port.

---

## Starting the Bridge

```bash
bridge
```

Transport mode is read from `common.toml` — no extra flags needed.

---

## What Happens at Startup

1. The bridge detects your MagicDNS hostname (e.g. `my-laptop.tail1234.ts.net`).
2. `tailscale serve --https=443 http://localhost:<port>` is configured automatically.
3. The pairing URL uses `wss://my-laptop.tail1234.ts.net` — no certificate fingerprint needed because Tailscale provides a CA-signed certificate.
4. When the bridge exits, `tailscale serve reset` cleans up the serve configuration automatically.

---

## Pairing URL Format

```
wss://my-laptop.tail1234.ts.net
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

---

## Mobile Client Notes

- The mobile device must be connected to the same tailnet before pairing. Verify with the Tailscale app that the device can reach the bridge host.
- After pairing, the app reconnects automatically as long as the device remains on the tailnet.

---

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `tailscale: not found on PATH` | Tailscale not installed | Install from [tailscale.com/download](https://tailscale.com/download) |
| `Not enrolled in a Tailscale network` | Machine not connected | Run `tailscale up` |
| `tailscale-serve mode requires MagicDNS + HTTPS` | MagicDNS or HTTPS not enabled on tailnet | Enable in [Tailscale admin console](https://login.tailscale.com/admin/dns) → DNS |
| `tailscale serve requires Tailscale v1.38+` | Outdated Tailscale | Update Tailscale |
| App cannot reach bridge after pairing | Mobile not on the same tailnet | Open Tailscale app on the phone, ensure it's signed in and connected |

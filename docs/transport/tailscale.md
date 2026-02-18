# Tailscale Transport

Use Tailscale as the transport layer for the ACP bridge. This lets you connect from any device on your tailnet without exposing a port to the public internet.

## Prerequisites

- **Tailscale v1.38+** installed on the machine running the bridge
  - macOS: `brew install tailscale`
  - Linux: follow [tailscale.com/download](https://tailscale.com/download)
- Machine enrolled in a tailnet: `tailscale up`

---

## `serve` Mode (Recommended)

Tailscale handles HTTPS termination via `tailscale serve`. The bridge listens on plain HTTP; Tailscale proxies HTTPS port 443 to it.

### Additional requirements

- MagicDNS enabled on the tailnet
- HTTPS certificates enabled — see [Enabling HTTPS](https://tailscale.com/kb/1153/enabling-https)

### Command

```bash
bridge start \
  --tailscale serve \
  --agent-command "copilot --acp" \
  --qr
```

### What happens

1. Bridge starts listening on `localhost:<port>` (plain HTTP).
2. `tailscale serve --https=443 http://localhost:<port>` is configured automatically.
3. The pairing URL uses `wss://<magicdns-hostname>` (no fingerprint needed — Tailscale provides a valid cert).
4. Scan the QR code from the mobile app.
5. When the bridge exits, `tailscale serve reset` is run automatically to clean up.

### Pairing URL format

```
wss://my-laptop.tail1234.ts.net
```

---

## `ip` Mode

The bridge binds directly to the Tailscale IP address (`100.x.x.x`) with self-signed TLS. The mobile app pins the certificate fingerprint for security.

### Command

```bash
bridge start \
  --tailscale ip \
  --agent-command "copilot --acp" \
  --qr
```

### What happens

1. Bridge detects the Tailscale IPv4 address and (if available) the MagicDNS hostname.
2. A self-signed TLS certificate is generated with the Tailscale IP (and hostname) as Subject Alternative Names.
3. The pairing URL uses `wss://<tailscale-ip-or-hostname>:<port>` with a certificate fingerprint.
4. Scan the QR code from the mobile app.

### Certificate regeneration

If your Tailscale address changes (e.g., you join a different tailnet), the bridge detects the change on next start and regenerates the certificate automatically. The mobile app will need to re-pair.

### Pairing URL format

```
wss://100.x.x.x:8080?fingerprint=AB:CD:...
```

---

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `tailscale not found on PATH` | Tailscale is not installed | Install from [tailscale.com/download](https://tailscale.com/download) |
| `Not enrolled in a Tailscale network` | Machine not connected to a tailnet | Run `tailscale up` |
| `tailscale serve mode requires MagicDNS + HTTPS` | MagicDNS or HTTPS not enabled | Enable in [Tailscale admin console](https://login.tailscale.com/admin/dns) |
| `tailscale serve requires Tailscale v1.38+` | Outdated Tailscale installation | Update Tailscale |
| Certificate changed warning on start | Tailscale IP changed since last cert generation | Expected — cert is regenerated; re-scan QR code on the mobile app |

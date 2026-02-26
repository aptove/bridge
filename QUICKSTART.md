# Quick Reference Guide

## Prerequisites

### Build from Source

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build the bridge
cd /path/to/bridge
cargo build --release

# Binary location: target/release/bridge
```

---

## Local Transport (Default)

No configuration needed. On first run, `common.toml` is created automatically with local transport enabled.

```bash
bridge run \
  --agent-command "gemini --experimental-acp" \
  --qr
```

Scan the QR code with the Aptove app.

---

## Cloudflare Transport

### 1. One-time setup

```bash
bridge setup \
  --api-token  "your_cloudflare_api_token" \
  --account-id "your_account_id" \
  --domain     "example.com" \
  --subdomain  "agent"
```

This provisions your Cloudflare tunnel, DNS record, and service token, and writes the credentials into `common.toml` under `[transports.cloudflare]`. You only run this once.

**Required API token permissions:**
- Zone → DNS → Edit
- Account → Cloudflare Tunnel → Edit
- Account → Access: Apps and Policies → Edit
- Account → Access: Service Tokens → Edit

**Your Cloudflare Account ID** can be found in the Cloudflare Dashboard URL or the right sidebar of your domain overview.

### 2. Enable Cloudflare in `common.toml`

After setup, verify `[transports.cloudflare]` has `enabled = true`:

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

### 3. Start

```bash
bridge run --agent-command "gemini --experimental-acp" --qr
```

---

## Tailscale Transport

See [docs/transport/tailscale.md](docs/transport/tailscale.md) for prerequisites. Enable either or both modes in `common.toml`:

```toml
[transports.tailscale-serve]   # HTTPS via MagicDNS (recommended)
enabled = true

[transports.tailscale-ip]      # Direct Tailscale IP with self-signed TLS
enabled = true
port    = 8765
tls     = true
```

Then start normally:

```bash
bridge run --agent-command "gemini --experimental-acp" --qr
```

---

## Running Multiple Transports Simultaneously

Enable multiple transport sections in `common.toml` — the bridge runs a listener for each:

```toml
[transports.local]
enabled = true
port    = 8765

[transports.cloudflare]
enabled = true
# ... cloudflare fields ...

[transports.tailscale-serve]
enabled = true
```

```bash
bridge run --agent-command "gemini --experimental-acp" --qr
```

All enabled transports start concurrently. The mobile app tries them in priority order (tailscale-serve → tailscale-ip → cloudflare → local) and connects via the first that succeeds.

---

## Useful Commands

### Show QR Code (second device)

```bash
bridge show-qr
```

Displays the connection QR for the currently active transport. **The bridge must already be running.** Use this to pair an additional device without restarting.

To show the QR at initial startup, pass `--qr` to `bridge run`:

```bash
bridge run --agent-command "aptove stdio" --qr
```

### Check Configuration Status

```bash
bridge status
```

Prints `agent_id`, `common.toml` path, enabled transports, and Tailscale availability.

---

## QR Code Payload

The QR encodes a pairing URL. The mobile app calls that URL to exchange the one-time code for credentials:

**Pairing URL:**
```
https://<IP>:<PORT>/pair/local?code=847291&fp=SHA256%3A...
```

**Pairing response (returned by the bridge):**
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

`agentId` is a stable UUID that lets the mobile app recognise the same agent across multiple transports — scanning a second transport's QR adds a new endpoint instead of creating a duplicate agent.

---

## Configuration File (`common.toml`)

All settings live in one file. The path depends on how you run the bridge:

| Runtime | Platform | Default Path |
|---------|----------|-------------|
| `aptove run` (embedded) | macOS | `~/Library/Application Support/Aptove/common.toml` |
| `aptove run` (embedded) | Linux | `~/.config/Aptove/common.toml` |
| `bridge` (standalone binary) | macOS | `~/Library/Application Support/com.aptove.bridge/common.toml` |
| `bridge` (standalone binary) | Linux | `~/.config/bridge/common.toml` |

When using `aptove run`, the config is shared across all workspaces.

Override with `--config-dir`:

```bash
bridge --config-dir ./my-config start --agent-command "gemini --experimental-acp"
```

### Rotating Credentials

To generate a new `agent_id`, `auth_token`, and TLS certificate (invalidates all paired devices):

```bash
# Using aptove run (embedded bridge):
rm ~/Library/Application\ Support/Aptove/common.toml   # macOS
rm ~/.config/Aptove/common.toml                        # Linux
aptove run --qr

# Using standalone bridge binary:
rm ~/Library/Application\ Support/com.aptove.bridge/common.toml   # macOS
rm ~/.config/bridge/common.toml                                    # Linux
bridge run --agent-command "aptove stdio" --qr
```

---

## Common Issues

### Agent fails to start
- Test the command manually: `gemini --experimental-acp`
- Ensure the agent binary is installed and on your `PATH`

### QR code not scanning
- Increase terminal font size or zoom in
- Copy the pairing URL shown below the QR and open it manually with the app

### "Zone not found" (Cloudflare setup)
- Check that the domain uses Cloudflare nameservers: `dig NS yourdomain.com`

### "Failed to create tunnel" (Cloudflare setup)
- Verify Zero Trust is enabled in the Cloudflare dashboard
- Check API token permissions (see Cloudflare Transport section above)
- Ensure a payment method is added (even for the free tier)

### "403 Forbidden" from mobile (Cloudflare)
- Ensure `CF-Access-Client-Id` and `CF-Access-Client-Secret` headers are being sent
- Check the service token hasn't been revoked in the Cloudflare dashboard

### Tailscale: "tailscale serve mode requires MagicDNS + HTTPS"
- Enable HTTPS certificates in the Tailscale admin console: Settings → DNS → Enable HTTPS

---

## Commands Summary

| Command | Purpose |
|---------|---------|
| `bridge run --agent-command <CMD>` | Run the bridge (reads transport config from `common.toml`) |
| `bridge show-qr` | Show QR / start offline registration |
| `bridge status` | Show configuration and transport status |
| `bridge setup ...` | Provision Cloudflare infrastructure (one-time) |

## Start Flags

| Flag | Description | Default |
|------|-------------|---------|
| `--agent-command <CMD>` | Command to spawn the ACP agent | Required |
| `--bind <ADDR>` | Bind address for all listeners | `0.0.0.0` |
| `--qr` | Display QR code(s) at startup | Off |
| `--verbose` | Info-level logging | Off |

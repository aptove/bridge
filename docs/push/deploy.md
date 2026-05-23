# Deploying the Push Notification Services

Two Cloudflare Workers must be deployed before push notifications work:

1. **cf-token** (`token.aptove.com`) — JWT issuance and client registry
2. **cf-push-relay** (`push.aptove.com`) — device token storage and APNs/FCM dispatch

**Deploy cf-token first.** The push relay fetches its JWKS on startup and caches it in KV.

---

## Prerequisites

```bash
node --version    # must be ≥ 18
wrangler --version  # install: npm i -g wrangler
wrangler login    # opens browser — authenticate once
```

---

## Part 1 — Deploy cf-token

### 1. Install dependencies
```bash
cd cf-token
npm install
```

### 2. Generate RSA key pair
```bash
node scripts/generate-keys.mjs
```
Copy the printed PEM block (everything from `-----BEGIN PRIVATE KEY-----` to `-----END PRIVATE KEY-----`). Keep it in a text editor — you'll paste it in step 5.

### 3. Create the KV namespace
```bash
wrangler kv namespace create CLIENT_REGISTRY
```
Copy the `id` value from the output. Open `wrangler.toml` and replace the placeholder:
```toml
[[kv_namespaces]]
binding = "CLIENT_REGISTRY"
id = "<paste id here>"
```

### 4. Set the admin client ID
In `wrangler.toml`, change `ADMIN_CLIENT_ID` from the default `"admin"` to something non-guessable:
```toml
[vars]
ADMIN_CLIENT_ID = "admin-aptove-2026"   # your choice — write it down
```

### 5. Set secrets
```bash
wrangler secret put RS_PRIVATE_KEY
# paste the full PEM from step 2, press Enter, then Ctrl+D

wrangler secret put ADMIN_CLIENT_SECRET
# type a strong random password (e.g. output of: openssl rand -hex 32)
```

### 6. Deploy
```bash
wrangler deploy
```

### 7. Verify
```bash
curl https://token.aptove.com/health
# → {"ok":true,"status":"healthy","timestamp":"..."}

curl https://token.aptove.com/.well-known/jwks.json
# → {"keys":[{"kty":"RSA","use":"sig","alg":"RS256","kid":"...","n":"...","e":"AQAB"}]}
```

---

## Part 2 — Deploy cf-push-relay

### 1. Install dependencies
```bash
cd ../cf-push-relay
npm install
```

### 2. KV namespaces
The `wrangler.toml` already has IDs for `DEVICE_TOKENS` and `AUTH_TOKENS`. If this is a fresh Cloudflare account (those IDs belong to a different account), recreate them:
```bash
wrangler kv namespace create DEVICE_TOKENS
wrangler kv namespace create AUTH_TOKENS
```
Then update the two `id` fields in `wrangler.toml`.

### 3. Set APNs secrets
Get these from **Apple Developer → Certificates, Identifiers & Profiles → Keys**:
```bash
wrangler secret put APNS_PRIVATE_KEY
# paste the full contents of the .p8 file, then Ctrl+D

wrangler secret put APNS_KEY_ID
# 10-character Key ID shown next to the key in Apple Developer portal

wrangler secret put APNS_TEAM_ID
# 10-character Team ID from Apple Developer → Membership
```

### 4. Set FCM secrets (Android — skip if iOS only)
From **Firebase Console → Project Settings → Service Accounts → Generate new private key** (downloads a JSON file):
```bash
wrangler secret put FCM_PRIVATE_KEY
# paste the "private_key" field value from the JSON (the RSA key block)

wrangler secret put FCM_CLIENT_EMAIL
# paste the "client_email" field value from the JSON
```
Also set `FCM_PROJECT_ID` in `wrangler.toml` under `[vars]`:
```toml
FCM_PROJECT_ID = "your-firebase-project-id"
```

### 5. Check vars
In `wrangler.toml`, confirm:
```toml
[vars]
APNS_BUNDLE_ID    = "com.aptove.ios"
APNS_SANDBOX      = "true"            # change to "false" for App Store / TestFlight
TOKEN_SERVICE_URL = "https://token.aptove.com"
```

### 6. Deploy
```bash
wrangler deploy
```

### 7. Verify
```bash
curl https://push.aptove.com/health
# → {"ok":true,"status":"healthy"}
```

---

## Part 3 — Create a Bridge Client

### 1. Get an admin JWT
```bash
ADMIN_JWT=$(curl -s -X POST https://token.aptove.com/token \
  -H "Content-Type: application/json" \
  -d '{"client_id":"admin-aptove-2026","client_secret":"<ADMIN_CLIENT_SECRET>"}' \
  | jq -r .access_token)

echo $ADMIN_JWT   # should be a long JWT string, not null
```

### 2. Create a push:write client for the bridge
```bash
curl -s -X POST https://token.aptove.com/clients \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H "Content-Type: application/json" \
  -d '{"client_id":"bridge-home","scope":"push:write"}'
```
Output:
```json
{ "ok": true, "client_id": "bridge-home", "client_secret": "<64 hex chars>" }
```
**Save `client_secret` now — it cannot be retrieved again.**

### 3. Add to bridge common.toml
```toml
[push_relay]
url           = "https://push.aptove.com"
token_url     = "https://token.aptove.com"
client_id     = "bridge-home"
client_secret = "<secret from above>"
```

Restart the bridge. Logs should show:
```
INFO  Push relay: JWT auth (client_id=bridge-home, relay=https://push.aptove.com)
```

---

## Part 4 — End-to-End Smoke Test

```bash
# Get a push:write JWT
TOKEN=$(curl -s -X POST https://token.aptove.com/token \
  -H "Content-Type: application/json" \
  -d '{"client_id":"bridge-home","client_secret":"<secret>"}' \
  | jq -r .access_token)

# Register a test device (use a real APNs token from a debug iOS build)
curl -X POST https://push.aptove.com/register \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"device_token":"<apns-token>","platform":"ios","bundle_id":"com.aptove.ios"}'

# Send a test notification
curl -X POST https://push.aptove.com/push \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"title":"Test","body":"Push relay is live"}'
```

---

## Custom Domains

Both workers use custom domains configured in their `wrangler.toml` (`token.aptove.com`, `push.aptove.com`). Cloudflare provisions DNS and TLS automatically when the domain is on the same Cloudflare account — no extra steps needed after `wrangler deploy`.

If the domain is **not** on Cloudflare, temporarily comment out the `routes` entry in `wrangler.toml` and access the worker via its `workers.dev` subdomain:
```
https://token-service.<your-account>.workers.dev
https://push-relay.<your-account>.workers.dev
```

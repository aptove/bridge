# Push Notifications System

The bridge implements a secure push notification system that enables mobile apps (iOS and Android) to receive real-time alerts when agents have new activity, even when the app is in the background or the device is asleep.

## Architecture Overview

```
┌──────────────────┐         ┌──────────────────┐        ┌─────────────────────┐
│   Mobile App     │         │     Bridge       │        │   cf-token Worker   │
│  (iOS/Android)   │         │  (Your Machine)  │        │  token.aptove.com   │
└──────────────────┘         └──────────────────┘        └─────────────────────┘
         │                            │                             │
         │  1. Scan QR Code           │                             │
         │──────────────────────────► │                             │
         │    (pushRelayUrl present   │                             │
         │     → push is configured) │                             │
         │                            │  2. POST /token             │
         │                            │    {client_id,              │
         │                            │──────────────────────────►  │
         │                            │     client_secret}          │
         │                            │  ◄── RS256 JWT (1h TTL) ──  │
         │                            │                             │
         │  3. Request Device Token   │                             │
         │    from OS (APNs / FCM)    │                             │
         │◄───────────────────────────│ (OS prompt triggered by app)│
         │                            │                             │
         │  4. bridge/registerPushToken│                            │
         │    via WebSocket           │                             │
         │──────────────────────────► │                             │
         │    {deviceToken, platform} │                             │
         │                            │
         │           ┌────────────────┴──────────────────┐
         │           │       cf-push-relay Worker        │
         │           │       push.aptove.com             │
         │           └────────────────┬──────────────────┘
         │                            │  5. POST /register
         │                            │──────────────────────────►
         │                            │    Authorization: Bearer <jwt>
         │                            │    {device_token, platform}
         │                            │
         │                            │  6. Store Mapping
         │                            │◄──────────────────────────
         │                            │    devices:<client_id> → [device_tokens]
         │                            │
         │                            │
         │    [ App goes to background / closes ]
         │                            │
         │                            │  7. POST /push (agent has activity)
         │                            │──────────────────────────►
         │                            │    Authorization: Bearer <jwt>
         │                            │    {title, body}
         │                            │
         │  8. Notification via APNs/FCM                 │
         │◄──────────────────────────────────────────────
         │    Notification appears!
```

## Why Three Services?

### The Problem

Bridges run on user machines. Two credentials must never reach user machines:
- **APNs credentials** — Apple `.p8` private key, Key ID, Team ID
- **FCM credentials** — Google service account JSON (RSA private key)

Distributing these to every bridge would be a critical security risk.

### The Solution: cf-push-relay

The relay centralizes APNs/FCM credential management. Bridges call the relay over HTTPS; the relay forwards to Apple and Google. The bridge never holds push credentials.

### The New Problem: Relay Authorization

With the relay open to any bridge, how does it isolate one bridge's device tokens from another's? The original approach used the bridge's `auth_token` as a shared secret ("relay_token") in the request body. This had two problems:

1. **No cryptographic verification** — any caller knowing a relay_token could send push notifications to that bridge's devices
2. **Cannot support multiple tenants** — if you host the relay for many customers, there's no way to provision per-bridge access without sharing the relay's implementation details

### The Solution: cf-token (JWT M2M Auth)

cf-token is a lightweight Cloudflare Worker that acts as an OAuth2-style M2M token issuer:

1. Each bridge has a unique `client_id` and `client_secret` (provisioned once by the relay operator)
2. The bridge exchanges these credentials for a short-lived RS256 JWT (1h TTL) at `POST /token`
3. The bridge includes `Authorization: Bearer <jwt>` on every push relay request
4. The relay verifies the JWT signature against the JWKS published by cf-token
5. The JWT `sub` claim (the bridge's `client_id`) is used as the isolation key for device storage

No relay_token is in any request body. No shared secret is passed over the wire — only signed JWTs.

## Key Components

### 1. cf-token (token.aptove.com)
- Issues RS256 JWTs to authenticated bridge clients
- Publishes JWKS at `/.well-known/jwks.json` (public, cached 1h)
- Stores client registry in Cloudflare KV (hashed secrets, PBKDF2-SHA256)
- Admin endpoints to create and revoke bridge clients
- RSA-2048 private key stored as Cloudflare Secret (never in code or KV)

### 2. cf-push-relay (push.aptove.com)
- Accepts device token registration and push requests from bridges
- Verifies RS256 Bearer JWTs against cf-token JWKS (KV-cached, rotation-safe)
- Stores device tokens in KV under `devices:<client_id>`
- Forwards notifications to APNs (iOS) and FCM (Android)
- Refreshes APNs JWT and FCM OAuth2 token via cron every 45 minutes

### 3. Bridge (this crate)
- Reads `[push_relay]` config from `common.toml`
- Fetches a JWT from cf-token on first push request; caches it until <60s remain
- Intercepts `bridge/registerPushToken` WebSocket notifications from mobile apps
- Forwards device token registration to cf-push-relay using its JWT
- Sends push notifications when agents produce output with no connected client
- Implements 30-second per-bridge debouncing to prevent notification spam

### 4. Mobile App (iOS/Android)
- Receives `pushRelayUrl` from the pairing QR response (signals push is configured)
- Requests notification permission from the OS
- Obtains APNs/FCM device token from the OS
- Sends device token to bridge via `bridge/registerPushToken` WebSocket notification
- **Never contacts cf-push-relay or cf-token directly**
- **Never knows the bridge's `client_id`, `client_secret`, or JWT**

## Device Isolation

Each bridge has a unique `client_id`. Device tokens are stored under `devices:<client_id>`. Push notifications are sent to `devices:<client_id>`. Because the `client_id` comes from a cryptographically signed JWT, no bridge can impersonate another or access its devices.

```
Bridge Instance A (client_id: bridge-alice)    Bridge Instance B (client_id: bridge-bob)
  │                                               │
  ├─ Fetches JWT (sub: bridge-alice)              ├─ Fetches JWT (sub: bridge-bob)
  │                                               │
  ├─ Registers device → devices:bridge-alice      ├─ Registers device → devices:bridge-bob
  │                                               │
  └─ Sends push  → looks up devices:bridge-alice  └─ Sends push  → looks up devices:bridge-bob

KV Store (cf-push-relay):
┌───────────────────┬────────────────────────────────────────────────┐
│  Key              │  Value                                         │
├───────────────────┼────────────────────────────────────────────────┤
│ devices:bridge-alice │ [{platform:"ios", token:"aaa..."}, ...]    │
│ devices:bridge-bob   │ [{platform:"android", token:"bbb..."}, ...] │
└───────────────────┴────────────────────────────────────────────────┘
```

Bridge A cannot send to Bridge B's devices — the JWT signature cannot be forged.

## Security Model

### What the mobile app shares, and with whom

| Data | Recipient | Channel |
|------|-----------|---------|
| APNs/FCM device token | Bridge only | Authenticated WebSocket |
| Nothing | cf-push-relay | Mobile never contacts the relay |
| Nothing | cf-token | Mobile never contacts the token service |

The mobile only needs the `pushRelayUrl` from the pairing response to know that push is configured (so it requests notification permission and triggers registration). All relay communication is performed by the bridge using its own credentials.

### Credential boundaries

```
┌─────────────────────────────────────────────────────────────────────────┐
│                   What each party holds                                  │
├──────────────────┬──────────────────────────────────────────────────────┤
│ Mobile app       │ APNs/FCM device token (from OS)                      │
│                  │ pushRelayUrl (from QR pairing)                       │
├──────────────────┼──────────────────────────────────────────────────────┤
│ Bridge           │ client_id + client_secret (from common.toml)         │
│                  │ Cached RS256 JWT (1h TTL, fetched from cf-token)     │
├──────────────────┼──────────────────────────────────────────────────────┤
│ cf-token         │ RSA-2048 private key (Cloudflare Secret)             │
│                  │ Admin credentials (Cloudflare Secret + vars)         │
│                  │ Hashed client secrets (KV, PBKDF2-SHA256)            │
├──────────────────┼──────────────────────────────────────────────────────┤
│ cf-push-relay    │ APNs private key, Key ID, Team ID (Cloudflare Secrets)│
│                  │ FCM private key, client email (Cloudflare Secrets)   │
│                  │ JWKS cache + device token mappings (KV)              │
└──────────────────┴──────────────────────────────────────────────────────┘
```

## Bridge Configuration

Push notifications are configured entirely via `common.toml`. All four fields are required; push is silently disabled if the section is absent or any field is empty.

```toml
[push_relay]
url           = "https://push.aptove.com"   # cf-push-relay base URL
token_url     = "https://token.aptove.com"  # cf-token base URL
client_id     = "bridge-home-office"        # provisioned via POST /clients on cf-token
client_secret = "<secret shown once at creation>"
```

Bridge clients are provisioned once by the relay operator using the cf-token admin API (see `cf-token/README.md`).

## Pairing Flow

When a mobile app scans the bridge QR code, the bridge returns a JSON pairing response. If push is fully configured, the response includes `pushRelayUrl`:

```json
{
  "url": "wss://192.168.1.100:3001",
  "protocol": "acp",
  "version": "1.0.0",
  "authToken": "...",
  "certFingerprint": "SHA256:...",
  "pushRelayUrl": "https://push.aptove.com"
}
```

The mobile app uses `pushRelayUrl` as a signal: if present, request notification permission from the OS and register the device token with the bridge. If absent, skip push entirely — no hardcoded relay URL exists in the mobile app.

## Message Flow

### JWT Acquisition (bridge-internal, automatic)

Before the first push relay request (and when the cached JWT is within 60s of expiry):

```
Bridge → POST https://token.aptove.com/token
         Content-Type: application/json
         { "client_id": "bridge-home-office", "client_secret": "..." }

cf-token → { "access_token": "<jwt>", "token_type": "Bearer", "expires_in": 3600 }

Bridge caches JWT until (now + 3600s - 60s), then refreshes proactively.
```

### Device Registration

**1. Mobile app → Bridge (WebSocket, JSON-RPC notification):**
```json
{
  "method": "bridge/registerPushToken",
  "params": {
    "deviceToken": "<APNs or FCM device token>",
    "platform": "apns",
    "bundleId": "com.aptove.ios"
  }
}
```

**2. Bridge → cf-push-relay (HTTP):**
```
POST https://push.aptove.com/register
Authorization: Bearer <jwt>
Content-Type: application/json

{
  "device_token": "<APNs or FCM device token>",
  "platform": "ios",
  "bundle_id": "com.aptove.ios"
}
```

**3. cf-push-relay stores:** `devices:bridge-home-office → [{ platform: "ios", token: "..." }]`

### Sending a Push Notification

Triggered automatically when an agent produces output and no WebSocket client is connected:

```
POST https://push.aptove.com/push
Authorization: Bearer <jwt>
Content-Type: application/json

{ "title": "Agent Name", "body": "New activity" }
```

cf-push-relay looks up `devices:<client_id>` from the JWT `sub` and dispatches to APNs/FCM.

### Device Unregistration

**Mobile app → Bridge (WebSocket):**
```json
{
  "method": "bridge/unregisterPushToken",
  "params": {
    "deviceToken": "<device token to remove>"
  }
}
```

**Bridge → cf-push-relay:**
```
DELETE https://push.aptove.com/register
Authorization: Bearer <jwt>
Content-Type: application/json

{ "device_token": "<device token to remove>" }
```

## Debouncing

To prevent notification spam, the bridge debounces push notifications per bridge (30-second cooldown):

```
T+0s:  Agent responds → Notification sent
T+10s: Agent responds → Dropped (20s remaining)
T+20s: Agent responds → Dropped (10s remaining)
T+35s: Agent responds → Notification sent
```

The debounce key is the bridge's `client_id`. Message buffering is enabled in the agent pool (`buffer_messages: true`, `max_buffer_size: 10_000`), so messages produced while the mobile is disconnected are replayed when it reconnects — push notifications are the wake-up signal, not the data carrier.

## Error Handling

Push notifications are non-critical. Failures are logged but never surface to the agent or break the bridge session.

| Scenario | Bridge behavior |
|----------|-----------------|
| cf-token unreachable | Log error, skip notification |
| JWT expired / invalid | Re-fetch JWT, retry once |
| cf-push-relay unreachable | Log warning, skip notification |
| No devices registered | Relay returns success, no-op |
| APNs/FCM rejects stale token | Relay removes token automatically |
| Debounce cooldown active | Drop silently |

## Implementation Structure

```
bridge/src/
├── push.rs           PushRelayClient
│   ├── new()            Constructor (relay_url)
│   ├── with_jwt_credentials()  Set client_id/secret/token_url
│   ├── get_jwt()        Fetch or return cached JWT
│   ├── register_device()  POST /register with Bearer JWT
│   ├── unregister_device() DELETE /register with Bearer JWT
│   └── notify()         POST /push with debouncing
│
├── common_config.rs  PushRelayConfig struct + CommonConfig.push_relay
│
├── main.rs           Reads [push_relay] config, constructs PushRelayClient,
│                     sets relay_url on PairingManager
│
├── bridge.rs         Intercepts bridge/registerPushToken and
│                     bridge/unregisterPushToken WebSocket notifications
│
├── pairing.rs        PairingResponse.relay_url → JSON "pushRelayUrl"
│                     PairingManager.with_relay_url()
│
└── agent_pool.rs     buffer_messages: true, max_buffer_size: 10_000
```

## Testing

### Setup

1. Deploy cf-token and cf-push-relay (see their respective READMEs)
2. Create a bridge client via cf-token admin API
3. Add `[push_relay]` section to `common.toml`
4. Build and run the bridge — check logs for `Push relay: JWT auth (client_id=...)`

### Manual relay test with curl

```bash
# 1. Fetch a push:write JWT
TOKEN=$(curl -s -X POST https://token.aptove.com/token \
  -H "Content-Type: application/json" \
  -d '{"client_id":"bridge-home-office","client_secret":"<secret>"}' \
  | jq -r .access_token)

# 2. Register a test device
curl -X POST https://push.aptove.com/register \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"device_token":"test-token","platform":"ios","bundle_id":"com.aptove.ios"}'

# 3. Send a test notification
curl -X POST https://push.aptove.com/push \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"title":"Test","body":"Push relay is working"}'
```

### Bruno collections

- `cf-token/bruno/` — JWT lifecycle (health, JWKS, get admin token, create client, get push token, delete client)
- `cf-push-relay/bruno/` — Push relay operations (register, push, unregister) using `{{access_token}}`

## Monitoring

```bash
# Bridge logs (--verbose)
# INFO  Push relay: JWT auth (client_id=bridge-home-office, relay=https://push.aptove.com)
# INFO  Fetched push relay JWT (expires in 3600s)
# INFO  Registered ios device token with push relay
# INFO  Sending push notification via relay
# DEBUG Push notification debounced (15s remaining)

# Relay logs
cd cf-push-relay && npx wrangler tail
cd cf-token     && npx wrangler tail
```

## Key Rotation

To rotate the RS256 key pair without downtime:

1. `node cf-token/scripts/generate-keys.mjs` — generate new RSA-2048 key
2. `wrangler secret put RS_PRIVATE_KEY` — update secret in cf-token
3. `wrangler deploy` in `cf-token/` — new isolates load new key; JWKS `kid` changes
4. cf-push-relay detects the unknown `kid` on the next request, invalidates its JWKS KV cache, and fetches the updated JWKS automatically — no relay redeployment needed
5. Already-issued JWTs (up to 1h old) will fail verification after rotation; bridges will re-fetch automatically on the next request

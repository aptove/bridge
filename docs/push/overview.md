# Push Notifications System

The bridge implements a secure push notification system that enables mobile apps (iOS and Android) to receive real-time alerts when agents have new activity, even when the app is in the background or the device is asleep.

## Architecture Overview

```
┌──────────────────┐                 ┌─────────────────┐                 ┌──────────────────┐
│   Mobile App     │                 │     Bridge      │                 │   Push Relay     │
│  (iOS/Android)   │                 │  (Your Machine) │                 │ (Cloudflare CDN) │
└──────────────────┘                 └─────────────────┘                 └──────────────────┘
         │                                    │                                    │
         │  1. Scan QR Code                   │                                    │
         │─────────────────────────────────►  │                                    │
         │    (auth_token + relay_url)        │                                    │
         │                                    │                                    │
         │  2. Request Device Token from OS   │                                    │
         │    (APNs for iOS / FCM for Android)│                                    │
         │◄───────────────────────────────────│                                    │
         │                                    │                                    │
         │  3. Register Device Token          │                                    │
         │    via WebSocket                   │                                    │
         │─────────────────────────────────►  │                                    │
         │    bridge/registerPushToken        │                                    │
         │                                    │                                    │
         │                                    │  4. Forward Registration           │
         │                                    │─────────────────────────────────►  │
         │                                    │    POST /register                  │
         │                                    │    {                               │
         │                                    │      relay_token: auth_token,      │
         │                                    │      device_token: "...",          │
         │                                    │      platform: "ios"               │
         │                                    │    }                               │
         │                                    │                                    │
         │                                    │  5. Store Mapping                  │
         │                                    │◄────────────────────────────────── │
         │                                    │    relay_token → [device_tokens]   │
         │                                    │                                    │
         │                                    │                                    │
         │  [ User interacts, agent responds ] │                                   │
         │                                    │                                    │
         │                                    │  6. Send Push Notification         │
         │                                    │─────────────────────────────────►  │
         │                                    │    POST /push                      │
         │                                    │    {                               │
         │                                    │      relay_token: auth_token,      │
         │                                    │      title: "Agent Name",          │
         │                                    │      body: "New activity"          │
         │                                    │    }                               │
         │                                    │                                    │
         │  7. Push to APNs/FCM              │                                    │
         │◄──────────────────────────────────────────────────────────────────────│
         │    Notification appears!           │                                    │
         │                                    │                                    │
```

## Why a Relay Service?

The bridge never holds APNs (Apple Push Notification service) credentials or FCM (Firebase Cloud Messaging) credentials. Instead:

1. **Security**: Distributing APNs `.p8` keys or FCM service account JSON files to every bridge would be a critical security risk
2. **Simplicity**: Bridge instances only need to know the relay URL and use their existing `auth_token`
3. **Isolation**: Each bridge's `auth_token` serves as its `relay_token`, ensuring devices are isolated per bridge
4. **Centralization**: The relay centralizes credential management and handles the complexity of JWT generation and OAuth2 tokens

## Key Components

### 1. Push Relay (Cloudflare Worker)
- Hosted at `https://push.aptove.com`
- Stores APNs and FCM credentials as encrypted secrets
- Maintains mapping: `relay_token` → list of device tokens
- Forwards notifications to Apple APNs and Google FCM
- Auto-refreshes authentication tokens (JWT for APNs, OAuth2 for FCM)

### 2. Bridge (This Crate)
- Generates secure `auth_token` during startup
- Uses `auth_token` as `relay_token` when communicating with push relay
- Accepts device token registration from mobile apps
- Forwards registration to push relay
- Sends push notifications when agents have activity
- Implements debouncing (30s cooldown) to prevent notification spam

### 3. Mobile App (iOS/Android)
- Scans QR code to get `auth_token` and `relay_url`
- Requests device token from OS (APNs for iOS, FCM for Android)
- Registers device token with bridge via WebSocket
- Receives push notifications when agent has activity
- Can unregister when disconnecting

## Security Model

### Authentication Token Isolation
```
Bridge Instance A                Bridge Instance B
    │                                │
    ├─ auth_token: ABC...            ├─ auth_token: XYZ...
    │                                │
    ├─ relay_token: ABC...           ├─ relay_token: XYZ...
    │                                │
    └─ Devices:                      └─ Devices:
       • iPhone (token: 111...)          • Android (token: 888...)
       • iPad (token: 222...)            • Pixel (token: 999...)

Push Relay Storage (KV):
┌──────────────────────────────────────────────────────┐
│  Key (relay_token)  │  Value (device tokens)         │
├────────────────────┼────────────────────────────────┤
│  ABC...             │  [                             │
│                     │    {platform:"ios",token:"111"},│
│                     │    {platform:"ios",token:"222"} │
│                     │  ]                             │
├────────────────────┼────────────────────────────────┤
│  XYZ...             │  [                             │
│                     │    {platform:"android",         │
│                     │     token:"888"},              │
│                     │    {platform:"android",         │
│                     │     token:"999"}               │
│                     │  ]                             │
└────────────────────┴────────────────────────────────┘
```

**Key Properties:**
- Bridge A cannot send notifications to Bridge B's devices
- Each bridge's devices are isolated by the `auth_token`
- The relay never needs to authenticate the bridge - the token itself provides isolation
- Lost/stolen bridge instances can be blocked at the relay by invalidating their `relay_token`

### Credential Protection
```
┌────────────────────────────────────────────────────────────┐
│                    APNs/FCM Credentials                     │
│                  (NEVER leave the relay)                    │
├────────────────────────────────────────────────────────────┤
│  • APNs Private Key (.p8 file)                             │
│  • APNs Key ID                                              │
│  • APNs Team ID                                             │
│  • FCM Private Key (RSA from service account JSON)         │
│  • FCM Client Email                                         │
│                                                             │
│  Stored as: Cloudflare Worker Secrets (encrypted at rest)  │
│  Access: Relay worker code only                            │
│  Rotation: Via `wrangler secret put` CLI                   │
└────────────────────────────────────────────────────────────┘
```

## QR Code Contents

When the bridge runs with `--qr`, it displays a QR code containing a JSON payload:

### QR Code Data Structure

```json
{
  "url": "wss://192.168.1.100:3001",
  "protocol": "acp",
  "version": "1.0",
  "authToken": "base64-encoded-token-min-32-chars-xxxxxxxxxxxxxxxx",
  "certFingerprint": "SHA256:ABCD1234...",
  "relayUrl": "https://push.aptove.com"
}
```

### Field Descriptions

| Field | Type | Description |
|-------|------|-------------|
| `url` | string | WebSocket URL to connect to the bridge |
| `protocol` | string | Protocol identifier, always "acp" |
| `version` | string | ACP protocol version |
| `authToken` | string | Bridge authentication token (≥32 chars)<br>**Used as `relay_token` for push notifications** |
| `certFingerprint` | string | SHA256 fingerprint of TLS certificate (local mode only) |
| `relayUrl` | string | Push relay service URL<br>**New field for push notifications** |

### What's Different with Push Support?

**Before push notifications:**
```json
{
  "url": "wss://192.168.1.100:3001",
  "protocol": "acp",
  "version": "1.0",
  "authToken": "auth-token-here",
  "certFingerprint": "SHA256:..."
}
```

**With push notifications:**
```json
{
  "url": "wss://192.168.1.100:3001",
  "protocol": "acp",
  "version": "1.0",
  "authToken": "auth-token-here",
  "certFingerprint": "SHA256:...",
  "relayUrl": "https://push.aptove.com"  ← NEW
}
```

The mobile app uses:
- `url` + `authToken` → Connect to bridge WebSocket
- `authToken` + `relayUrl` → Register for push notifications (as `relay_token`)

## Bridge Configuration

### Starting with Push Notifications

```bash
bridge run \
  -a "copilot --acp" \
  --port 3001 \
  --stdio-proxy \
  --push-relay-url https://push.aptove.com
```

### Command-Line Arguments

| Argument | Default | Description |
|----------|---------|-------------|
| `--push-relay-url <URL>` | `https://push.oss.aptov.com` | Push relay service URL |

### Environment Variables

None specifically for push (uses existing `auth_token` from bridge session).

## Message Flow

### Device Registration

**1. Mobile app sends to bridge (WebSocket):**
```json
{
  "method": "bridge/registerPushToken",
  "params": {
    "deviceToken": "apns-device-token-or-fcm-token",
    "platform": "ios",
    "bundleId": "com.aptove.ios"
  }
}
```

**2. Bridge forwards to relay (HTTP):**
```bash
POST https://push.aptove.com/register
Content-Type: application/json

{
  "relay_token": "bridge-auth-token",
  "device_token": "apns-device-token-or-fcm-token",
  "platform": "ios",
  "bundle_id": "com.aptove.ios"
}
```

**3. Relay responds:**
```json
{
  "ok": true,
  "message": "Device registered"
}
```

**4. Bridge confirms to mobile app:**
```json
{
  "result": {
    "success": true
  }
}
```

### Sending Push Notifications

**When agent has new activity:**

**1. Bridge sends to relay:**
```bash
POST https://push.aptove.com/push
Content-Type: application/json

{
  "relay_token": "bridge-auth-token",
  "title": "GitHub Copilot",
  "body": "Your agent has new activity"
}
```

**2. Relay sends to APNs/FCM:**

**For iOS (APNs):**
```bash
POST https://api.push.apple.com/3/device/{device_token}
Authorization: bearer {JWT_TOKEN}
apns-topic: com.aptove.ios

{
  "aps": {
    "alert": {
      "title": "GitHub Copilot",
      "body": "Your agent has new activity"
    },
    "sound": "default",
    "badge": 1
  }
}
```

**For Android (FCM):**
```bash
POST https://fcm.googleapis.com/v1/projects/{project_id}/messages:send
Authorization: Bearer {OAUTH2_TOKEN}

{
  "message": {
    "token": "{device_token}",
    "notification": {
      "title": "GitHub Copilot",
      "body": "Your agent has new activity"
    }
  }
}
```

**3. Device receives notification:**
- Notification appears in system tray
- User can tap to open app
- App reconnects to bridge if needed

## Debouncing Strategy

To prevent notification spam, the bridge implements per-relay-token debouncing:

```rust
// Default cooldown: 30 seconds
const COOLDOWN: Duration = Duration::from_secs(30);

// Debounce tracking
HashMap<relay_token, last_notification_time>
```

**Behavior:**
- First notification: Sent immediately
- Subsequent notifications within 30s: Silently dropped
- After 30s cooldown: Next notification sent

**Example timeline:**
```
T+0s:  Agent responds → Notification sent ✅
T+10s: Agent responds → Dropped (20s remaining) ❌
T+20s: Agent responds → Dropped (10s remaining) ❌
T+35s: Agent responds → Notification sent ✅
```

## Device Unregistration

**Mobile app sends to bridge:**
```json
{
  "method": "bridge/unregisterPushToken",
  "params": {
    "deviceToken": "device-token-to-remove"
  }
}
```

**Bridge forwards to relay:**
```bash
DELETE https://push.aptove.com/register
Content-Type: application/json

{
  "relay_token": "bridge-auth-token",
  "device_token": "device-token-to-remove"
}
```

## Error Handling

### Registration Errors

| Scenario | Bridge Behavior | Mobile App Action |
|----------|-----------------|-------------------|
| Relay is down | Log warning, return error to app | Show error, allow retry |
| Invalid device token | Relay rejects, bridge returns error | Regenerate token, retry |
| Network timeout (10s) | Return timeout error | Retry with backoff |

### Notification Errors

| Scenario | Bridge Behavior | User Impact |
|----------|-----------------|-------------|
| Relay is down | Log warning, continue | No notification (silent failure) |
| No devices registered | Relay returns success | None (expected) |
| APNs/FCM rejects token | Relay removes stale token | Future notifications work |
| Debounced | Drop notification | Reduces spam |

**Philosophy: Push notifications are non-critical**
- Failure to send a notification should never break the core bridge functionality
- Mobile app should not rely solely on push notifications for updates
- WebSocket connection is the primary communication channel

## Testing

### Local Testing Setup

1. **Start bridge with push relay:**
```bash
cd bridge
cargo build --release
./target/release/bridge run \
  -a "copilot --acp" \
  --port 3001 \
  --stdio-proxy \
  --push-relay-url https://push.aptove.com \
  --verbose
```

2. **Scan QR code** with mobile app

3. **Register device token:**
```json
{
  "method": "bridge/registerPushToken",
  "params": {
    "deviceToken": "test-token-from-mobile-os",
    "platform": "ios"
  }
}
```

4. **Trigger agent activity** to test notifications

### Manual Relay Testing

Test the relay directly with curl:

```bash
# 1. Register a test device
curl -X POST https://push.aptove.com/register \
  -H "Content-Type: application/json" \
  -d '{
    "relay_token": "test-token-min-32-chars-xxxxx",
    "device_token": "your-apns-or-fcm-token",
    "platform": "ios"
  }'

# 2. Send test notification
curl -X POST https://push.aptove.com/push \
  -H "Content-Type: application/json" \
  -d '{
    "relay_token": "test-token-min-32-chars-xxxxx",
    "title": "Test Notification",
    "body": "Testing push notifications"
  }'

# 3. Health check
curl https://push.aptove.com/health
```

## Implementation Details

### Bridge Code Structure

```
bridge/src/
├── push.rs              # PushRelayClient implementation
│   ├── register_device()   # Register device token with relay
│   ├── unregister_device() # Remove device token from relay
│   └── notify()            # Send push notification (with debouncing)
│
├── main.rs              # CLI argument parsing
│   └── --push-relay-url    # Relay URL configuration
│
├── session.rs           # WebSocket message handling
│   ├── bridge/registerPushToken   # Handle registration from app
│   └── bridge/unregisterPushToken # Handle unregistration from app
│
└── pairing.rs           # QR code generation
    └── relayUrl field   # Include relay URL in QR code
```

### Relay Code Structure (Cloudflare Worker)

```
cf-push-relay/src/
├── index.ts       # Main worker entry point
├── router.ts      # HTTP request routing
├── apns.ts        # Apple Push Notification service
├── fcm.ts         # Firebase Cloud Messaging
├── kv.ts          # Cloudflare KV storage
└── types.ts       # TypeScript interfaces
```

## Monitoring and Debugging

### Bridge Logs

```bash
# Enable verbose logging
bridge run --verbose ...

# Sample log output:
INFO  📱 Registering ios device token with push relay
DEBUG Push relay URL: https://push.aptove.com/register
INFO  ✅ Device token registered with push relay
INFO  🔔 Sending push notification via relay for agent 'GitHub Copilot'
DEBUG Push notification throttled (15s remaining)
INFO  ✅ Push notification sent via relay
```

### Relay Logs

```bash
# Tail Cloudflare Worker logs
cd cf-push-relay
npx wrangler tail

# Sample output:
[INFO] POST /register - ios device registered for relay_token: abc...
[INFO] POST /push - sending to 1 device(s) for relay_token: abc...
[INFO] APNs response: 200 OK
```

## Best Practices

1. **Always use HTTPS** for relay URL in production
2. **Implement retry logic** in mobile apps for registration
3. **Handle token refresh** when OS regenerates device tokens
4. **Test with real devices** - simulators don't support push notifications
5. **Monitor relay health** - set up alerts for downtime
6. **Rotate credentials** periodically (APNs keys, FCM service accounts)
7. **Use sandbox mode** for development (set `APNS_SANDBOX=true` in relay)

## Troubleshooting

### No notifications received

1. Check device is registered:
   ```bash
   # Bridge logs should show
   INFO ✅ Device token registered with push relay
   ```

2. Verify relay is reachable:
   ```bash
   curl https://push.aptove.com/health
   ```

3. Check debouncing isn't blocking:
   ```bash
   # Bridge logs show
   DEBUG Push notification throttled (Xs remaining)
   ```

4. Verify APNs/FCM credentials in relay:
   ```bash
   npx wrangler secret list
   ```

### Registration fails

1. Check relay URL is correct in bridge runup
2. Verify network connectivity
3. Check relay logs for errors:
   ```bash
   npx wrangler tail
   ```

### Notifications delayed

- Expected: APNs/FCM may delay notifications when device is in power-saving mode
- Check debouncing isn't too aggressive (30s default)
- Verify relay cron job is running (refreshes auth tokens every 45 min)

## Future Enhancements

- [ ] Support rich notifications (images, actions)
- [ ] Custom notification sounds
- [ ] Per-agent notification preferences
- [ ] Notification history/badges
- [ ] Silent notifications for background sync
- [ ] Rate limiting by device (not just relay_token)
- [ ] Analytics dashboard for notification delivery

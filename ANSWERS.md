# Answers to Your Questions

## Q1: Do I need a dedicated subdomain and Cloudflare token?

**Yes**, and here's exactly what you need:

### Subdomain
- **Type**: Single-level (e.g., `agent.yourdomain.com`)
- **Why**: Cloudflare's free Universal SSL certificate covers single-level subdomains
- **Multi-level**: `my.agent.yourdomain.com` requires paid Advanced Certificate Manager
- **Recommendation**: Use a dedicated subdomain like `agent`, `acp`, or `bridge`

### API Token
You need a **scoped API Token** (not Global API Key) with these specific permissions:

| Service | Resource | Permission |
|---------|----------|------------|
| Cloudflare One | Connectors | Edit |
| Access | Apps and Policies | Edit |
| Access | Service Tokens | Edit |
| Zone | DNS | Edit |

**How to create**:
1. Go to Cloudflare Dashboard → My Profile → API Tokens
2. Click "Create Token"
3. Select "Create Custom Token"
4. Add the permissions above
5. Set Zone Resources to your specific domain
6. Create and save the token (shown only once!)

The Rust CLI will use this token to:
- Create the tunnel
- Add DNS records
- Set up Access policies
- Generate Service Tokens

## Q2: Can free accounts implement this?

**Yes, absolutely!** Cloudflare's free tier is sufficient for this entire setup.

### What's Included on Free Tier

✅ **Cloudflare Tunnels**: Unlimited named tunnels
✅ **Zero Trust Access**: Up to 50 users (seats)
✅ **Service Tokens**: Full support for authentication
✅ **DNS Management**: Unlimited records
✅ **Universal SSL**: Covers single-level subdomains
✅ **API Access**: Complete programmatic control

### What's NOT Included (But Not Needed)

❌ **Advanced Certificate Manager**: Only needed for multi-level subdomains
❌ **Extended Log Retention**: Free tier keeps 24 hours (sufficient)
❌ **WAF Rules**: Limited, but not needed for this use case
❌ **Teams Plans**: Advanced features not required

### Requirements

Even on the free tier, you must:
1. ✅ Own a domain and point it to Cloudflare nameservers
2. ✅ Add a payment method (for identity verification)
   - You won't be charged for free tier usage
   - Just prevents abuse/spam accounts
3. ✅ Enable Zero Trust in the dashboard

## Q3: Can the CLI tool setup Cloudflare Zero Trust?

**Yes, completely!** The `bridge setup` command automates everything.

### What Gets Automated

#### 1. Tunnel Creation
```bash
POST /accounts/{account_id}/cfd_tunnel
```
Creates a named tunnel with a unique ID and secret.

#### 2. DNS Configuration
```bash
POST /zones/{zone_id}/dns_records
```
Creates a CNAME record pointing your subdomain to the tunnel.

#### 3. Access Application
```bash
POST /accounts/{account_id}/access/apps
```
Creates a Zero Trust application protecting your hostname.

#### 4. Service Auth Policy
```bash
POST /accounts/{account_id}/access/apps/{app_id}/policies
```
Adds a policy requiring Service Token authentication.

#### 5. Service Token Generation
```bash
POST /accounts/{account_id}/access/service_tokens
```
Generates Client ID and Secret for mobile authentication.

#### 6. Ingress Rules
```bash
PUT /accounts/{account_id}/cfd_tunnel/{tunnel_id}/configurations
```
Configures how traffic is routed to your local bridge.

### What You Run

Just one command:
```bash
bridge setup \
  --api-token "your_token" \
  --account-id "your_account_id" \
  --domain "yourdomain.com" \
  --subdomain "agent"
```

### What You Get

1. ✅ Tunnel created and configured
2. ✅ DNS record `agent.yourdomain.com` → tunnel
3. ✅ Zero Trust policy protecting the endpoint
4. ✅ Service Token for mobile authentication
5. ✅ Configuration saved locally
6. ✅ QR code displayed for mobile setup

**Zero manual steps in the Cloudflare Dashboard!**

## Q4: Are there other limitations?

### Hard Requirements

1. **Domain Ownership**
   - You must own a domain (can be cheap .xyz, .dev, etc.)
   - Domain must use Cloudflare nameservers
   - Cannot use "Quick Tunnels" (trycloudflare.com) for Service Auth

2. **Payment Method**
   - Required for Zero Trust verification
   - Won't be charged on free tier
   - Just validates you're a real person

3. **Single-Level Subdomains**
   - Free tier SSL: `agent.domain.com` ✅
   - Requires paid: `my.agent.domain.com` ❌

### Soft Limitations

4. **Log Retention**
   - Free tier: 24 hours
   - Usually sufficient for debugging
   - Consider external logging if needed

5. **Connection Limits**
   - 50 user seats on free tier
   - More than enough for personal use
   - Each mobile device = 1 connection

6. **Tunnel Bandwidth**
   - No explicit limit on free tier
   - Subject to "fair use"
   - For AI agents (mostly text), you'll never hit it

### Technical Considerations

7. **Cloudflared Daemon**
   - The tunnel daemon must run alongside your bridge
   - Can run on same machine or different one
   - Consider: systemd service, Docker, or manual process

8. **Security**
   - Service Tokens are long-lived (default: 1 year)
   - Should implement token rotation
   - Consider end-to-end encryption for sensitive data

9. **Platform Support**
   - Rust CLI: macOS, Linux, Windows (with WSL)
   - Agent: depends on agent's platform support
   - Mobile: iOS, Android (WebSocket support required)

## Implementation Advantages

### Why This Architecture Wins

1. **No Port Forwarding**: Cloudflare Tunnel handles everything
2. **Global Access**: Connect from anywhere
3. **Zero Configuration**: Mobile users just scan QR
4. **Secure by Default**: Zero Trust + Service Tokens
5. **Low Latency**: Cloudflare's edge network
6. **Free Forever**: All features on free tier
7. **No VPN Required**: Direct connection through tunnel

### Comparison to Alternatives

| Approach | Cost | Setup | Security | Global Access |
|----------|------|-------|----------|---------------|
| Port Forwarding | Free | Complex | Risky | Maybe |
| VPN | $5-50/mo | Complex | Good | Yes |
| ngrok | $8-25/mo | Easy | Good | Yes |
| Cloudflare (This) | **Free** | **Automated** | **Excellent** | **Yes** |

## Complete Answer Summary

**Your Questions → Answers**

1. ✅ **Subdomain + Token**: Yes, both required and automated
2. ✅ **Free Account**: Yes, works perfectly on free tier
3. ✅ **CLI Setup**: Yes, completely automated via API
4. ✅ **Limitations**: 
   - Domain required (can be cheap)
   - Payment method for verification
   - Single-level subdomains on free tier
   - 50 user seats (more than enough)

**Bottom Line**: You can build a production-ready, globally-accessible, secure bridge between stdio ACP agents and mobile apps using only:
- Cloudflare's free tier
- A domain (~$10/year)
- This Rust CLI tool (open source)
- Zero ongoing costs

## Next Steps

1. **Get a Domain**: If you don't have one, buy a cheap one (~$1-10/year)
2. **Add to Cloudflare**: Point nameservers to Cloudflare
3. **Enable Zero Trust**: Add payment method (won't be charged)
4. **Create API Token**: With the 4 permissions listed above
5. **Build the CLI**: `cargo build --release`
6. **Run Setup**: `bridge setup ...`
7. **Scan QR**: From your mobile app
8. **Start Bridge**: `bridge start ...`
9. **Connect**: Mobile → Cloudflare → Bridge → Agent

**Total cost**: Domain only (~$10/year). Everything else is free! 🎉

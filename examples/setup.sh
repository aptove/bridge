# Example: Setting up the bridge with Cloudflare Zero Trust

# 1. Setup Cloudflare (one-time)
#    This creates the tunnel, DNS record, Access Application, and Service Token.
#    It also writes ~/.cloudflared/<tunnel-id>.json and ~/.cloudflared/config.yml
#    so that cloudflared can be started automatically by the bridge.
export CLOUDFLARE_API_TOKEN="your_token_here"
export CLOUDFLARE_ACCOUNT_ID="your_account_id_here"

cargo run --release -- setup \
  --api-token "$CLOUDFLARE_API_TOKEN" \
  --account-id "$CLOUDFLARE_ACCOUNT_ID" \
  --domain "yourdomain.com" \
  --subdomain "agent"

# 2. Start the bridge with managed Cloudflare tunnel
#    --cloudflare spawns `cloudflared tunnel run` automatically.
#    Requires cloudflared to be installed: brew install cloudflare/cloudflare/cloudflared
cargo run --release -- start \
  --agent-command "gemini --experimental-acp" \
  --cloudflare \
  --qr

# Example: Setting up the bridge

# 1. Setup Cloudflare (one-time)
export CLOUDFLARE_API_TOKEN="your_token_here"
export CLOUDFLARE_ACCOUNT_ID="your_account_id_here"

cargo run --release -- setup \
  --domain "yourdomain.com" \
  --subdomain "agent"

# 2. Start the bridge
cargo run --release -- start \
  --agent-command "gemini --experimental-acp" \
  --qr

# 3. In another terminal, start the Cloudflare tunnel daemon
# (This connects your local bridge to Cloudflare's network)
cloudflared tunnel --config ~/.cloudflared/config.yml run mobile-acp-bridge

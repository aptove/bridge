# Persistent Sessions

The ACP Bridge supports persistent sessions via the `--keep-alive` flag. When enabled, agent processes remain alive during temporary disconnections — such as network switches, app backgrounding, or device sleep — enabling seamless session resumption without losing conversation context.

## Overview

Without `--keep-alive`, the bridge terminates the agent process as soon as the WebSocket connection closes. With `--keep-alive`, the agent process continues running in a pool, keyed by the client's authentication token. When the same client reconnects, the bridge reattaches to the existing agent process instead of spawning a new one.

## How It Works

### Session Lifecycle

```
1. FIRST CONNECT
   Client ──WebSocket──► Bridge ──spawn──► Agent Process
                                           │
                         Bridge caches ◄───┘ initialize response

2. CLIENT DISCONNECTS
   Client ──closes──► Bridge marks agent as "idle"
                      Agent process stays alive in pool
                      (Optional) Bridge buffers agent output

3. CLIENT RECONNECTS
   Client ──WebSocket──► Bridge looks up agent by auth token
                         Bridge intercepts initialize request
                         Bridge returns cached initialize response
                         Client resumes with loadSession / prompt

4. IDLE TIMEOUT
   No reconnect within --session-timeout seconds
   Bridge terminates the idle agent process
```

### Agent Pool Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         AGENT POOL                              │
│                                                                 │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │  token_abc123 → Agent Process [status: connected]        │  │
│  │                 PID: 4821                                 │  │
│  │                 cached_init: ✓                            │  │
│  │                 connected_at: 2026-02-05T10:30:00Z        │  │
│  ├───────────────────────────────────────────────────────────┤  │
│  │  token_def456 → Agent Process [status: idle 5m]          │  │
│  │                 PID: 4835                                 │  │
│  │                 cached_init: ✓                            │  │
│  │                 disconnected_at: 2026-02-05T10:25:00Z     │  │
│  └───────────────────────────────────────────────────────────┘  │
│                                                                 │
│  Reaper task: runs every 60s, terminates agents idle beyond     │
│               --session-timeout                                 │
│                                                                 │
│  Limits: --max-agents (default 10)                              │
└─────────────────────────────────────────────────────────────────┘
```

### Initialize Interception

When a mobile app establishes a new WebSocket connection, the ACP SDK always sends an `initialize` JSON-RPC request as the first message. If this request were forwarded to an already-running agent, the agent would re-initialize and lose all conversation context.

The bridge solves this with **initialize interception**:

**First connection (agent is new):**
1. Client sends `initialize` → Bridge forwards it to the agent
2. Agent responds with its capabilities, server info, and agent info
3. Bridge captures and caches this response in the agent pool
4. Bridge forwards the response to the client

**Subsequent connections (agent is reused):**
1. Client sends `initialize` → Bridge intercepts it (does NOT forward)
2. Bridge reads the cached `initialize` response
3. Bridge swaps the JSON-RPC `id` to match the client's request
4. Bridge sends the modified cached response directly to the client
5. The agent process is unaware a new connection was made

```
First Connect:                          Reconnect:

Client ──init──► Bridge ──init──► Agent    Client ──init──► Bridge ──X──  Agent
Client ◄─resp─── Bridge ◄─resp─── Agent    Client ◄─cached── Bridge      (not forwarded)
                 Bridge caches resp         Bridge swaps JSON-RPC id
```

This ensures the agent process continues exactly where it left off, with full conversation history intact.

### Message Buffering

When `--buffer-messages` is enabled, the bridge captures any output the agent produces while no client is connected. When the client reconnects, buffered messages are replayed in order before live streaming resumes.

This is useful for agents that send asynchronous notifications or continue processing tasks after the client disconnects.

## Configuration

### CLI Flags

| Flag | Description | Default |
|------|-------------|---------|
| `--keep-alive` | Enable session persistence | Off |
| `--session-timeout <secs>` | How long idle agents stay alive after disconnect | `1800` (30 min) |
| `--max-agents <n>` | Maximum concurrent agent processes in the pool | `10` |
| `--buffer-messages` | Buffer agent output while client is disconnected | Off |

### Example Commands

```bash
# Basic session persistence (30-minute timeout)
./target/release/bridge start \
  --agent-command "copilot --acp" \
  --port 8080 \
  --stdio-proxy \
  --qr \
  --keep-alive

# Extended timeout with message buffering
./target/release/bridge start \
  --agent-command "copilot --acp" \
  --port 8080 \
  --stdio-proxy \
  --qr \
  --keep-alive \
  --session-timeout 3600 \
  --buffer-messages

# High-capacity server
./target/release/bridge start \
  --agent-command "copilot --acp" \
  --port 8080 \
  --stdio-proxy \
  --qr \
  --keep-alive \
  --max-agents 50 \
  --session-timeout 7200
```

## Mobile App Reconnection Flow

The following describes how the iOS Aptove app works with persistent sessions:

### First-Time Connection

1. User scans QR code → app extracts host, port, auth token, and TLS fingerprint
2. App stores the auth token in the iOS Keychain and agent metadata in UserDefaults
3. App opens a WebSocket connection with the `X-Bridge-Token` header
4. ACP SDK sends `initialize` → bridge forwards to newly spawned agent
5. ACP SDK sends `createSession` → agent creates a new session
6. User begins chatting via `prompt` requests

### Reconnection After Disconnect

1. App detects the WebSocket connection was lost (network change, backgrounding, etc.)
2. App reads the stored auth token from the Keychain
3. App opens a new WebSocket connection with the same `X-Bridge-Token` header
4. ACP SDK sends `initialize` → **bridge intercepts and returns cached response**
5. ACP SDK sends `loadSession` with the previously stored session ID
6. Agent loads the existing session — full conversation context is restored
7. User continues chatting seamlessly

### What Gets Preserved

| Component | Preserved? | How |
|-----------|-----------|-----|
| Agent process | ✅ Yes | Bridge keeps it alive in the pool |
| Conversation history | ✅ Yes | Agent process retains in-memory state |
| Session ID | ✅ Yes | Mobile app stores and resubmits it |
| Agent capabilities | ✅ Yes | Bridge caches and replays `initialize` response |
| Auth token | ✅ Yes | Stored in iOS Keychain, sent on reconnect |
| WebSocket connection | ❌ No | New TCP/TLS connection established each time |

## Security

### Token-Based Session Routing

Agent processes are keyed by the client's authentication token. A valid token is required to reconnect to an existing session. Each token maps to exactly one agent process — there is no cross-session access.

### Idle Timeout

Disconnected agents are automatically terminated after `--session-timeout` seconds (default: 30 minutes). This limits the window during which a stale session can be reconnected to and prevents resource leaks from abandoned sessions.

### Max-Agents Limit

The `--max-agents` flag caps the number of concurrent agent processes, preventing resource exhaustion from too many simultaneous sessions. When the pool is full, new connection attempts that would require spawning a new agent are rejected.

### Process Isolation

Each agent runs as a separate OS process with its own stdin/stdout pipes. There is no shared memory or IPC between agent processes. The bridge communicates with each agent exclusively through stdio.

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| Agent not reused after reconnect | Auth token mismatch between connections | Ensure the mobile app sends the same `X-Bridge-Token` header on reconnect |
| Agent killed immediately on disconnect | `--keep-alive` flag not set | Add `--keep-alive` to the start command |
| "Agent pool is full" error | All agent slots occupied | Increase `--max-agents` or disconnect unused sessions |
| Messages lost during disconnect | Message buffering not enabled | Add `--buffer-messages` flag |
| Agent dies while client is away | Process crashed or idle timeout too short | Increase `--session-timeout`; check agent stderr with `--verbose` |
| Reconnect works but conversation lost | Agent re-initialized on reconnect | Ensure bridge version includes initialize interception (v0.2+) |
| State lost despite process alive | Agent itself doesn't persist internal state | Agent-specific issue; bridge preserves the *process*, not app-level state |

### Debugging Session Persistence

```bash
# Enable verbose logging to see pool activity
./target/release/bridge start \
  --agent-command "copilot --acp" \
  --stdio-proxy --qr --keep-alive --verbose

# Watch for these log messages:
# "Reusing existing agent for token: abc..."  → successful reconnect
# "Spawning new agent for token: abc..."      → new agent (first connect or expired)
# "AgentPool stats: 2/10 agents (1 connected, 1 idle)"  → pool status (every 60s)
# "Intercepting initialize for reused agent"  → init interception working
# "Agent idle timeout, terminating: abc..."   → idle reaper triggered
```

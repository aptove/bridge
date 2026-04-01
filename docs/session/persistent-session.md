# Persistent Sessions

The ACP Bridge supports persistent sessions via an agent pool. Agent processes remain alive during temporary disconnections — such as network switches, app backgrounding, or device sleep — enabling seamless session resumption without losing conversation context.

The agent pool is always active. No CLI flags are needed.

## Overview

When a client disconnects, the bridge keeps the agent process alive in a pool keyed by the client's authentication token. When the same client reconnects, the bridge reattaches to the existing agent process instead of spawning a new one. Idle agents are automatically reaped after 30 minutes.

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
   No reconnect within idle_timeout
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
│  ├───────────────────────────────────────────────────────────┤  │
│  │  token_def456 → Agent Process [status: idle 5m]          │  │
│  │                 PID: 4835                                 │  │
│  │                 cached_init: ✓                            │  │
│  └───────────────────────────────────────────────────────────┘  │
│                                                                 │
│  Reaper task: runs every 60s, terminates agents idle beyond     │
│               idle_timeout (default: 30 min)                    │
│                                                                 │
│  Limits: max_agents (default 10)                                │
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

When `buffer_messages` is enabled in `PoolConfig`, the bridge captures any output the agent produces while no client is connected. When the client reconnects, buffered messages are replayed in order before live streaming resumes.

## Configuration

The pool runs with the following defaults (defined in `PoolConfig`):

| Setting | Value | Description |
|---------|-------|-------------|
| Idle timeout | 30 minutes | How long idle agents stay alive after disconnect |
| Max agents | 10 | Maximum concurrent agent processes in the pool |
| Buffer messages | off | Buffer agent output while client is disconnected |
| Reaper interval | 60 seconds | How often the background reaper checks for idle agents |

## Mobile App Reconnection Flow

The following describes how the iOS Aptove app works with persistent sessions:

### First-Time Connection

1. User scans QR code → app extracts host, port, auth token, and TLS fingerprint
2. App stores the auth token in the iOS Keychain and agent metadata in CoreData
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
| Agent process | Yes | Bridge keeps it alive in the pool |
| Conversation history | Yes | Agent process retains in-memory state |
| Session ID | Yes | Mobile app stores and resubmits it |
| Agent capabilities | Yes | Bridge caches and replays `initialize` response |
| Auth token | Yes | Stored in iOS Keychain, sent on reconnect |
| WebSocket connection | No | New TCP/TLS connection established each time |

## Security

### Token-Based Session Routing

Agent processes are keyed by the client's authentication token. A valid token is required to reconnect to an existing session. Each token maps to exactly one agent process — there is no cross-session access.

### Idle Timeout

Disconnected agents are automatically terminated after the configured idle timeout (default: 30 minutes). This limits the window during which a stale session can be reconnected to and prevents resource leaks from abandoned sessions.

### Process Isolation

Each agent runs as a separate OS process with its own stdin/stdout pipes. There is no shared memory or IPC between agent processes. The bridge communicates with each agent exclusively through stdio.

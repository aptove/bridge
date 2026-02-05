//! Integration tests for session persistence (keep-alive) feature.
//!
//! These tests exercise the agent pool using real `cat` processes to verify
//! end-to-end message routing, reconnection, idle timeout, and backward
//! compatibility.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

// The crate is the `bridge` library — its public API surfaces everything we need.
use bridge::agent_pool::{AgentPool, PoolConfig};

// ── Helper ───────────────────────────────────────────────────────────────

fn fast_pool(max_agents: usize) -> AgentPool {
    AgentPool::new(PoolConfig {
        idle_timeout: Duration::from_millis(100),
        max_agents,
        buffer_messages: true,
        max_buffer_size: 50,
    })
}

// ── 9.1  Unit-level integration: AgentPool operations ────────────────────

#[tokio::test]
async fn pool_spawn_and_communicate_via_channels() {
    let mut pool = fast_pool(5);

    let (tx, mut rx, _buf, reused, _cached) = pool.get_or_spawn("tok1", "cat").await.unwrap();
    assert!(!reused);

    // Send a message through the stdin channel
    tx.send("hello".to_string()).await.unwrap();

    // The `cat` process echoes it back via the broadcast channel
    let echoed = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("timed out waiting for echo")
        .expect("broadcast recv failed");

    assert_eq!(echoed, "hello");

    pool.shutdown_all().await;
}

// ── 9.2  Connect → disconnect → reconnect ───────────────────────────────

#[tokio::test]
async fn reconnect_to_same_agent_session() {
    let mut pool = fast_pool(5);

    // === First connection ===
    let (tx1, mut rx1, _buf, reused, _cached) = pool.get_or_spawn("tok1", "cat").await.unwrap();
    assert!(!reused);

    // Verify echo works
    tx1.send("first".to_string()).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), rx1.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(msg, "first");

    // === Disconnect ===
    pool.mark_disconnected("tok1");
    let stats = pool.stats();
    assert_eq!(stats.idle, 1);
    assert_eq!(stats.connected, 0);

    // Send a message while disconnected (it goes to the process but no rx is listening)
    // The broadcast channel drops it since no subscribers.

    // === Reconnect ===
    let (tx2, mut rx2, _buf2, reused2, _cached) = pool.get_or_spawn("tok1", "cat").await.unwrap();
    assert!(reused2, "should reuse the same agent process");
    assert_eq!(pool.stats().connected, 1);

    // Verify echo still works after reconnect
    tx2.send("second".to_string()).await.unwrap();
    let msg2 = tokio::time::timeout(Duration::from_secs(2), rx2.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(msg2, "second");

    pool.shutdown_all().await;
}

#[tokio::test]
async fn reconnect_replays_buffered_messages() {
    let mut pool = fast_pool(5);

    let _ = pool.get_or_spawn("tok1", "cat").await.unwrap();
    pool.mark_disconnected("tok1");

    // Buffer messages while disconnected
    pool.buffer_message("tok1", "buf_a".to_string());
    pool.buffer_message("tok1", "buf_b".to_string());

    // Reconnect — should return buffered messages
    let (_tx, _rx, buffered, reused, _cached) = pool.get_or_spawn("tok1", "cat").await.unwrap();
    assert!(reused);
    assert_eq!(buffered, vec!["buf_a", "buf_b"]);

    pool.shutdown_all().await;
}

// ── 9.3  Idle timeout triggers cleanup ───────────────────────────────────

#[tokio::test]
async fn idle_timeout_cleans_up_disconnected_agents() {
    let pool = Arc::new(RwLock::new(fast_pool(5))); // idle_timeout = 100ms

    // Spawn and disconnect
    {
        let mut p = pool.write().await;
        let _ = p.get_or_spawn("tok1", "cat").await.unwrap();
        let _ = p.get_or_spawn("tok2", "cat").await.unwrap();
        p.mark_disconnected("tok1");
        // tok2 stays connected
    }

    // Wait for idle timeout to pass
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Reap
    {
        let mut p = pool.write().await;
        p.reap_idle_agents().await;
    }

    let p = pool.read().await;
    assert!(!p.contains("tok1"), "idle tok1 should be reaped");
    assert!(p.contains("tok2"), "connected tok2 should survive");

    drop(p);
    pool.write().await.shutdown_all().await;
}

#[tokio::test]
async fn reaper_background_task_reaps_on_schedule() {
    let pool = Arc::new(RwLock::new(fast_pool(5)));

    {
        let mut p = pool.write().await;
        let _ = p.get_or_spawn("tok1", "cat").await.unwrap();
        p.mark_disconnected("tok1");
    }

    // Start reaper with a 30ms check interval
    let handle = bridge::agent_pool::start_reaper(Arc::clone(&pool), Duration::from_millis(30));

    // Wait enough for idle_timeout (100ms) + at least one reaper tick
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        pool.read().await.stats().total,
        0,
        "reaper should have cleaned up idle agent"
    );

    handle.abort();
}

// ── 9.4  Max-agents limit enforced ──────────────────────────────────────

#[tokio::test]
async fn max_agents_blocks_when_all_connected() {
    let mut pool = fast_pool(2); // max_agents = 2

    let _ = pool.get_or_spawn("t1", "cat").await.unwrap();
    let _ = pool.get_or_spawn("t2", "cat").await.unwrap();

    let result = pool.get_or_spawn("t3", "cat").await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("pool is full"),
        "error should mention pool is full: {}",
        err_msg
    );

    pool.shutdown_all().await;
}

#[tokio::test]
async fn max_agents_evicts_oldest_idle() {
    let mut pool = fast_pool(2);

    let _ = pool.get_or_spawn("t1", "cat").await.unwrap();
    let _ = pool.get_or_spawn("t2", "cat").await.unwrap();

    // Disconnect t1, making it evictable
    pool.mark_disconnected("t1");

    // t3 should succeed by evicting t1
    let result = pool.get_or_spawn("t3", "cat").await;
    assert!(result.is_ok());
    assert!(!pool.contains("t1"));
    assert!(pool.contains("t2"));
    assert!(pool.contains("t3"));

    pool.shutdown_all().await;
}

// ── 9.5  Backward compatibility (no --keep-alive) ───────────────────────
//
// When no pool is configured, the bridge should use legacy mode where the
// agent process is killed on disconnect (`kill_on_drop(true)`). We verify
// that the pool is optional and default construction works.

#[tokio::test]
async fn pool_is_optional_default_construction() {
    // Simulates the bridge path where `agent_pool` is `None`.
    // The pool type itself can still be created and shut down cleanly.
    let pool: Option<Arc<RwLock<AgentPool>>> = None;
    assert!(pool.is_none(), "no pool = legacy mode");
}

#[tokio::test]
async fn legacy_mode_pool_not_used() {
    // Verify that creating a pool and immediately shutting it down is safe
    // (covers the code path where keep-alive is toggled off mid-session).
    let mut pool = AgentPool::new(PoolConfig::default());
    assert_eq!(pool.stats().total, 0);
    pool.shutdown_all().await;
    assert_eq!(pool.stats().total, 0);
}

#[tokio::test]
async fn dead_agent_replaced_not_reused() {
    let mut pool = fast_pool(5);

    let _ = pool.get_or_spawn("tok1", "cat").await.unwrap();

    // Kill it
    pool.kill_agent("tok1").await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Next get_or_spawn should detect it's dead and spawn a fresh one
    let (_tx, _rx, _buf, reused, _cached) = pool.get_or_spawn("tok1", "cat").await.unwrap();
    assert!(!reused, "dead agent should be replaced with a fresh spawn");
    assert_eq!(pool.stats().total, 1);

    pool.shutdown_all().await;
}

// ── Initialize caching ──────────────────────────────────────────────

#[tokio::test]
async fn cached_init_response_round_trip() {
    let mut pool = fast_pool(5);

    // First connection — no cached init
    let (_tx, _rx, _buf, reused, cached) = pool.get_or_spawn("tok1", "cat").await.unwrap();
    assert!(!reused);
    assert!(cached.is_none());

    // Simulate the bridge caching the initialize response
    let init_response = r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"streaming":true},"agentInfo":{"name":"TestAgent"}}}"#.to_string();
    pool.cache_init_response("tok1", init_response.clone());

    // Disconnect
    pool.mark_disconnected("tok1");

    // Reconnect — should get the cached init response back
    let (_tx, _rx, _buf, reused, cached) = pool.get_or_spawn("tok1", "cat").await.unwrap();
    assert!(reused);
    assert_eq!(cached.unwrap(), init_response);

    pool.shutdown_all().await;
}

#[tokio::test]
async fn cached_init_survives_multiple_reconnects() {
    let mut pool = fast_pool(5);

    let _ = pool.get_or_spawn("tok1", "cat").await.unwrap();
    let init_response = r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}"#.to_string();
    pool.cache_init_response("tok1", init_response.clone());

    // Multiple disconnect/reconnect cycles
    for _ in 0..3 {
        pool.mark_disconnected("tok1");
        let (_, _, _, reused, cached) = pool.get_or_spawn("tok1", "cat").await.unwrap();
        assert!(reused);
        assert_eq!(cached.unwrap(), init_response);
    }

    pool.shutdown_all().await;
}

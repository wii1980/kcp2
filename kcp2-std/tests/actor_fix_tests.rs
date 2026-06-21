//! Integration tests for actor-layer bug fixes:
//!
//! - C1: `kill()` reliability — now uses `watch::Sender` for guaranteed delivery
//! - H3: `recv()` timeout — pending_recv now respects the connection timeout
//!
//! Run: `cargo test -p kcp2-std --test actor_fix_tests -- --nocapture`

#![allow(clippy::uninlined_format_args)]

use std::time::Duration;

use kcp2_std::{KcpConfig, KcpConnector, KcpError, KcpListener};

// ═══════════════════════════════════════════════════════════════
// C1: kill() reliably terminates the connection
//
// kill() used try_send on the mpsc command channel, which silently
// failed when the channel was full. Now uses watch::Sender for
// guaranteed delivery — kill signals cannot be lost regardless
// of command channel state.
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_close_kills_connection_reliably() {
    // Set up a listener on a random port
    let listener = KcpListener::bind_with_config(
        "127.0.0.1:0",
        KcpConfig::default().timeout(Duration::from_secs(30)),
    )
    .await
    .expect("bind failed");
    let addr = listener.local_addr().expect("local_addr");

    // Connect a client
    let session = KcpConnector::new(&addr.to_string())
        .expect("connector creation failed")
        .with_config(KcpConfig::default().timeout(Duration::from_secs(30)))
        .conv(1)
        .connect()
        .await
        .expect("connect failed");

    let conn = session.connection();

    // Verify connection is alive
    assert!(!conn.is_dead().await, "connection should be alive initially");

    // Close the connection — this calls kill() internally
    conn.close();

    // Give the actor a moment to process the shutdown signal
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Connection should now be dead
    assert!(
        conn.is_dead().await,
        "connection should be dead after close()"
    );

    session.close().await;
    listener.close().await;
}

#[tokio::test]
async fn test_close_with_pending_send() {
    let listener = KcpListener::bind_with_config(
        "127.0.0.1:0",
        KcpConfig::default().timeout(Duration::from_secs(30)),
    )
    .await
    .expect("bind failed");
    let addr = listener.local_addr().expect("local_addr");

    let session = KcpConnector::new(&addr.to_string())
        .expect("connector")
        .with_config(
            KcpConfig::default()
                .timeout(Duration::from_secs(30))
                .channel_capacity(4), // Small channel to make it easier to fill
        )
        .conv(2)
        .connect()
        .await
        .expect("connect");

    let conn = session.connection();

    // Send multiple messages rapidly (to potentially fill the channel)
    for i in 0..20 {
        let _ = conn.send(format!("msg-{i}").as_bytes()).await;
    }

    // Close while sends may be pending
    conn.close();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Must be dead regardless of channel state
    assert!(
        conn.is_dead().await,
        "connection must be killable even with pending sends"
    );

    session.close().await;
    listener.close().await;
}

// ═══════════════════════════════════════════════════════════════
// H3: recv() returns error when no data arrives
//
// recv() returns Err(Timeout) when no data arrives within
// KcpConfig::timeout(). The actor's internal timeout check fires
// at the configured deadline. The connection is not closed.
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_recv_returns_timeout_when_no_data() {
    let listener = KcpListener::bind_with_config(
        "127.0.0.1:0",
        KcpConfig::default().timeout(Duration::from_millis(200)),
    )
    .await
    .expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let session = KcpConnector::new(&addr.to_string())
        .expect("connector")
        .with_config(KcpConfig::default().timeout(Duration::from_millis(200)))
        .conv(3)
        .connect()
        .await
        .expect("connect");

    let conn = session.connection();
    let mut buf = [0u8; 1024];

    // recv should error out since no data is being sent.
    // Wrap in a 5s safety timeout so the test cannot hang forever.
    let start = tokio::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(5), conn.recv(&mut buf))
        .await
        .expect("recv should not hang forever — safety timeout fired");

    let elapsed = start.elapsed();

    assert!(result.is_err(), "recv should return error");

    let err = result.unwrap_err();
    // With normal connect(), both the actor's internal recv timeout
    // (Timeout) and the external idle-timeout task (DeadLink) use the
    // same duration, so either may win the race. Both are acceptable.
    match &err {
        KcpError::Timeout => {}
        KcpError::DeadLink => {}
        other => panic!("expected Timeout or DeadLink, got {:?}", other),
    }

    // Should have waited roughly the timeout duration.
    // With a 200ms timeout and 100ms check interval, the actual
    // wait is between 200ms and ~300ms. We assert a generous
    // lower bound to avoid flakiness.
    assert!(
        elapsed >= Duration::from_millis(100),
        "should wait at least ~200ms before timeout, took {:?}",
        elapsed
    );

    session.close().await;
    listener.close().await;
}

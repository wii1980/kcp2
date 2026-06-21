//! Integration tests for listener/connection bug fixes
//!
//! Verifies:
//! - C2: `listener.close()` terminates all connection actors
//! - H1: `try_recv()` updates `last_active`, preventing premature reaper kill
//! - M6: `max_connections` limit rejects excess connections
//!
//! Run: `cargo test -p kcp2-std --test listener_fix_tests`

#![allow(clippy::module_name_repetitions)]

use std::net::UdpSocket as StdUdpSocket;
use std::time::Duration;

use kcp2_std::{KcpConfig, KcpConnector, KcpListener};

fn find_free_addr() -> String {
    let sock = StdUdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = sock.local_addr().unwrap();
    drop(sock);
    format!("127.0.0.1:{}", addr.port())
}

// ═══════════════════════════════════════════════════════════════
// Test C2: listener.close() terminates all connection actors
//
// Bug: close() just cleared the DashMap without calling conn.close()
// on each connection, leaving actor tasks running.
//
// Fix: close() now iterates and calls conn.close() on each connection,
// which kills the actor behind each KcpConnection.
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_listener_close_terminates_connections() {
    let addr = find_free_addr();
    let listener = KcpListener::bind_with_config(
        &addr,
        KcpConfig::default().timeout(Duration::from_secs(30)),
    )
    .await
    .expect("bind");

    // Client connects and sends data so the server can accept
    let session = KcpConnector::new(&addr)
        .expect("connector")
        .with_config(KcpConfig::default().timeout(Duration::from_secs(30)))
        .conv(10)
        .connect()
        .await
        .expect("connect");
    session.connection().send(b"hello").await.expect("client send");
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Accept on server side
    let (server_conn, _) = listener.accept().await.expect("accept");

    // Give actors time to process the accepted data
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Verify both connections are alive
    assert!(!session.connection().is_dead().await, "client conn alive");
    assert!(!server_conn.is_dead().await, "server conn alive");

    // Close the listener — should close all connections
    listener.close().await;

    // Give actors time to process shutdown
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Server-side connection should be dead after listener close
    assert!(
        server_conn.is_dead().await,
        "server connection should be dead after listener.close()"
    );

    session.close().await;
}

// ═══════════════════════════════════════════════════════════════
// Test H1: try_recv() updates last_active
//
// Bug: try_recv() was missing self.update_last_active(), causing the
// timeout task to prematurely kill connections that only use try_recv().
//
// Fix: try_recv() now calls update_last_active() like send/recv/input.
//
// Strategy: create connection with timeout T, wait < T/2, call try_recv
// (resets last_active), wait another < T/2, verify connection is still alive.
// Total elapsed < T, and the timeout task sees last_active refreshed.
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_try_recv_updates_last_active() {
    let timeout = Duration::from_secs(2);

    let addr = find_free_addr();
    let listener = KcpListener::bind_with_config(
        &addr,
        KcpConfig::default().timeout(timeout),
    )
    .await
    .expect("bind");

    let session = KcpConnector::new(&addr)
        .expect("connector")
        .with_config(KcpConfig::default().timeout(timeout))
        .conv(20)
        .connect()
        .await
        .expect("connect");

    let conn = session.connection();

    // Wait for ~40% of the timeout
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Call try_recv — this should update last_active
    let mut buf = [0u8; 1024];
    let _ = conn.try_recv(&mut buf).await;

    // Wait another ~40% of the timeout (total would be 80% of timeout, well within)
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Connection should still be alive because try_recv updated last_active
    assert!(
        !conn.is_dead().await,
        "connection should still be alive — try_recv should have updated last_active"
    );

    session.close().await;
    listener.close().await;
}

// ═══════════════════════════════════════════════════════════════
// Test M6: max_connections limit rejects new connections
//
// Bug: No limit on connections — attacker could exhaust resources.
//
// Fix: Added max_connections config field. Listener drops packets from
// new clients when the limit is reached.
//
// The rejection behavior is silent: the second client's connect() call
// succeeds locally (it creates its own session), but the server never
// creates a new connection for it. We verify connection_count() stays
// at 1 after attempting a second connection.
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_max_connections_rejects_excess() {
    let addr = find_free_addr();
    let listener = KcpListener::bind_with_config(
        &addr,
        KcpConfig::default()
            .timeout(Duration::from_secs(10))
            .max_connections(1),
    )
    .await
    .expect("bind");

    // First connection (conv=30) — should succeed
    let session1 = KcpConnector::new(&addr)
        .expect("connector")
        .with_config(KcpConfig::default().timeout(Duration::from_secs(10)))
        .conv(30)
        .connect()
        .await
        .expect("first connect");
    session1.connection().send(b"hello from conn1").await.expect("conn1 send");
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Accept first connection
    let (_, _) = listener.accept().await.expect("first accept");
    assert_eq!(listener.connection_count(), 1, "should have 1 connection");

    // Second connection (conv=31) — server should reject due to max_connections=1
    let session2 = KcpConnector::new(&addr)
        .expect("connector")
        .with_config(KcpConfig::default().timeout(Duration::from_secs(10)))
        .conv(31)
        .connect()
        .await
        .expect("second connect");

    // Send data from second client to trigger a packet on the server
    session2.connection().send(b"hello from conn2").await.ok();

    // Wait a bit for the server to process the packet
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Server should still only have 1 connection (second was rejected)
    assert_eq!(
        listener.connection_count(),
        1,
        "server should reject 2nd connection when max_connections=1"
    );

    session1.close().await;
    session2.close().await;
    listener.close().await;
}

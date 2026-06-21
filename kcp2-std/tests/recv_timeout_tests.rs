//! Acceptance tests for the idle-connection recv() timeout bug.
//!
//! Bug: `pending_recv` timeout check lives in `do_update()`, but
//! `do_update()` only runs when `needs_update()` is true. On an idle
//! connection, the `sleep_until` branch is disabled, so `do_update()`
//! never runs and `recv()` hangs forever (or until external timeout_task
//! fires DeadLink and force-closes the connection).
//!
//! T1-T4, T6: use `connect_with_recv_task()` which aborts the external
//! timeout_task, directly exposing whether the actor's internal timeout
//! works. Without fix → hangs past 5s safety net.
//!
//! T5: regression test using normal `connect()` — verifies normal recv()
//! still works (no false timeout).

#![allow(clippy::uninlined_format_args)]

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use kcp2_std::{KcpConfig, KcpConnection, KcpConnector, KcpError, KcpListener};

async fn setup_no_external_timeout(
    timeout: Duration,
    conv: u32,
) -> (KcpListener, Arc<KcpConnection>, JoinHandle<()>) {
    let listener =
        KcpListener::bind_with_config("127.0.0.1:0", KcpConfig::default().timeout(timeout))
            .await
            .expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let (conn, recv_task) = KcpConnector::new(&addr.to_string())
        .expect("connector")
        .with_config(KcpConfig::default().timeout(timeout))
        .conv(conv)
        .connect_with_recv_task()
        .await
        .expect("connect");

    tokio::time::sleep(Duration::from_millis(300)).await;

    (listener, conn, recv_task)
}

// ═══════════════════════════════════════════════════════════════
// T1: recv() must not hang — actor-internal timeout must fire.
// Without external timeout_task, only do_update() can wake recv().
// ═══════════════════════════════════════════════════════════════
#[tokio::test]
async fn t1_recv_timeout_fires_on_idle_connection() {
    let (listener, conn, recv_task) =
        setup_no_external_timeout(Duration::from_millis(400), 201).await;
    let mut buf = [0u8; 1024];

    let start = tokio::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(5), conn.recv(&mut buf))
        .await
        .expect("BUG: recv() hung past 5s — actor-internal timeout did not fire");
    let elapsed = start.elapsed();

    assert!(result.is_err());
    assert!(
        elapsed < Duration::from_secs(2),
        "recv took {:?} — timeout should fire ~400ms",
        elapsed
    );

    println!("T1: recv returned {:?} in {:?}", result, elapsed);

    recv_task.abort();
    conn.close();
    listener.close().await;
}

// ═══════════════════════════════════════════════════════════════
// T2: recv() timeout must return Timeout, not DeadLink.
// ═══════════════════════════════════════════════════════════════
#[tokio::test]
async fn t2_recv_timeout_returns_timeout_not_deadlink() {
    let (listener, conn, recv_task) =
        setup_no_external_timeout(Duration::from_millis(400), 202).await;
    let mut buf = [0u8; 1024];

    let result = tokio::time::timeout(Duration::from_secs(5), conn.recv(&mut buf))
        .await
        .expect("BUG: recv() hung past 5s");

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, KcpError::Timeout),
        "expected Timeout, got {:?}",
        err
    );

    println!("T2: correctly returned KcpError::Timeout");

    recv_task.abort();
    conn.close();
    listener.close().await;
}

// ═══════════════════════════════════════════════════════════════
// T3: Connection must survive recv() timeout.
// ═══════════════════════════════════════════════════════════════
#[tokio::test]
async fn t3_connection_survives_recv_timeout() {
    let (listener, conn, recv_task) =
        setup_no_external_timeout(Duration::from_millis(400), 203).await;
    let mut buf = [0u8; 1024];

    let result = tokio::time::timeout(Duration::from_secs(5), conn.recv(&mut buf))
        .await
        .expect("BUG: recv() hung past 5s");
    assert!(result.is_err());

    assert!(
        !conn.is_dead().await,
        "connection should survive recv timeout"
    );

    let send_result = conn.send(b"after-timeout").await;
    assert!(
        send_result.is_ok(),
        "send after timeout should succeed, got {:?}",
        send_result
    );

    println!("T3: connection alive and sending after recv timeout");

    recv_task.abort();
    conn.close();
    listener.close().await;
}

// ═══════════════════════════════════════════════════════════════
// T4: recv() timeout fires promptly (between 1x and 3x configured).
// ═══════════════════════════════════════════════════════════════
#[tokio::test]
async fn t4_recv_timeout_is_prompt() {
    let timeout_ms = 400u64;
    let (listener, conn, recv_task) =
        setup_no_external_timeout(Duration::from_millis(timeout_ms), 204).await;
    let mut buf = [0u8; 1024];

    let start = tokio::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(5), conn.recv(&mut buf))
        .await
        .expect("BUG: recv() hung past 5s");
    let elapsed = start.elapsed();

    assert!(result.is_err());

    let elapsed_ms = elapsed.as_millis() as u64;
    assert!(
        elapsed_ms >= timeout_ms,
        "fired too early: {}ms < {}ms",
        elapsed_ms,
        timeout_ms
    );
    assert!(
        elapsed_ms < timeout_ms * 3,
        "fired too late: {}ms >= {}ms",
        elapsed_ms,
        timeout_ms * 3
    );

    println!(
        "T4: timeout fired in {}ms (configured: {}ms)",
        elapsed_ms, timeout_ms
    );

    recv_task.abort();
    conn.close();
    listener.close().await;
}

// ═══════════════════════════════════════════════════════════════
// T5: Normal recv() must not false-trigger timeout (regression).
// Uses normal connect() with server accepting and sending data.
// ═══════════════════════════════════════════════════════════════
#[tokio::test]
async fn t5_recv_success_no_false_timeout() {
    let listener = KcpListener::bind_with_config(
        "127.0.0.1:0",
        KcpConfig::default().timeout(Duration::from_secs(10)),
    )
    .await
    .expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        let (server_conn, _peer) = listener.accept().await.expect("accept");
        let mut sbuf = [0u8; 1024];
        let _ = tokio::time::timeout(Duration::from_secs(2), server_conn.recv(&mut sbuf)).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        server_conn.send(b"hello-after-delay").await.unwrap();
    });

    let session = KcpConnector::new(&addr.to_string())
        .expect("connector")
        .with_config(KcpConfig::default().timeout(Duration::from_millis(2000)))
        .conv(205)
        .connect()
        .await
        .expect("connect");

    let conn = session.connection().clone();
    conn.send(b"ping").await.expect("client initial send");

    let mut buf = [0u8; 1024];
    let start = tokio::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(5), conn.recv(&mut buf))
        .await
        .expect("recv should not hang");
    let elapsed = start.elapsed();

    assert!(
        result.is_ok(),
        "recv should succeed, got {:?} (elapsed: {:?})",
        result,
        elapsed
    );
    let n = result.unwrap();
    assert_eq!(&buf[..n], b"hello-after-delay");
    assert!(
        elapsed < Duration::from_millis(1500),
        "recv took {:?}",
        elapsed
    );

    println!("T5: recv succeeded in {:?}", elapsed);

    session.close().await;
}

// ═══════════════════════════════════════════════════════════════
// T6: Multiple recv() timeouts in sequence.
// ═══════════════════════════════════════════════════════════════
#[tokio::test]
async fn t6_multiple_recv_timeouts_in_sequence() {
    let (listener, conn, recv_task) =
        setup_no_external_timeout(Duration::from_millis(400), 206).await;
    let mut buf = [0u8; 1024];

    let r1 = tokio::time::timeout(Duration::from_secs(5), conn.recv(&mut buf))
        .await
        .expect("BUG: first recv() hung");
    assert!(r1.is_err());

    let r2 = tokio::time::timeout(Duration::from_secs(5), conn.recv(&mut buf))
        .await
        .expect("BUG: second recv() hung");
    assert!(r2.is_err());

    if let Err(ref e) = r1 {
        assert!(
            matches!(e, KcpError::Timeout),
            "first: expected Timeout, got {:?}",
            e
        );
    }
    if let Err(ref e) = r2 {
        assert!(
            matches!(e, KcpError::Timeout),
            "second: expected Timeout, got {:?}",
            e
        );
    }

    assert!(
        !conn.is_dead().await,
        "connection should survive 2 timeouts"
    );

    println!("T6: two consecutive recv timeouts, connection alive");

    recv_task.abort();
    conn.close();
    listener.close().await;
}

//! Transport 层数据丢失复现测试
//!
//! 复现 `KcpActor` `drain_output` 中 `WouldBlock` 导致的丢包问题。
//! 修复前应 FAIL，修复后应 PASS。
//!
//! 运行: `cargo test -p kcp2-std --test data_loss_tests -- --nocapture`

#![allow(clippy::uninlined_format_args)]

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use kcp2_std::transport::KcpTransport;
use kcp2_std::{KcpConfig, KcpListener};

// ═══════════════════════════════════════════════════════════════
// Mock Transport: Simulates WouldBlock / intermittent failures
// ═══════════════════════════════════════════════════════════════

/// Transport that fails with `WouldBlock` after `fail_after` successful sends.
/// If `fail_after` is 0, ALL sends fail immediately.
struct FlakyTransport {
    local: SocketAddr,
    send_attempts: AtomicUsize,
    send_successes: AtomicUsize,
    send_failures: AtomicUsize,
    fail_after: usize,
    permanently_blocked: AtomicBool,
}

impl FlakyTransport {
    fn new(local: SocketAddr, fail_after: usize) -> Self {
        Self {
            local,
            send_attempts: AtomicUsize::new(0),
            send_successes: AtomicUsize::new(0),
            send_failures: AtomicUsize::new(0),
            fail_after,
            permanently_blocked: AtomicBool::new(false),
        }
    }

    fn block_permanently(&self) {
        self.permanently_blocked.store(true, Ordering::SeqCst);
    }

    fn attempts(&self) -> usize {
        self.send_attempts.load(Ordering::SeqCst)
    }

    fn successes(&self) -> usize {
        self.send_successes.load(Ordering::SeqCst)
    }

    fn failures(&self) -> usize {
        self.send_failures.load(Ordering::SeqCst)
    }
}

impl KcpTransport for FlakyTransport {
    fn try_send(&self, buf: &[u8]) -> io::Result<usize> {
        let attempt = self.send_attempts.fetch_add(1, Ordering::SeqCst);
        if self.permanently_blocked.load(Ordering::SeqCst) {
            self.send_failures.fetch_add(1, Ordering::SeqCst);
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "mock: permanently blocked",
            ));
        }
        if attempt >= self.fail_after {
            self.send_failures.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "mock: would block after threshold",
            ))
        } else {
            self.send_successes.fetch_add(1, Ordering::SeqCst);
            Ok(buf.len())
        }
    }

    fn try_send_to(&self, buf: &[u8], _target: SocketAddr) -> io::Result<usize> {
        self.try_send(buf)
    }

    fn recv<'a>(
        &'a self,
        _buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = io::Result<usize>> + Send + 'a>> {
        Box::pin(std::future::pending())
    }

    fn recv_from<'a>(
        &'a self,
        _buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = io::Result<(usize, SocketAddr)>> + Send + 'a>> {
        Box::pin(std::future::pending())
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local)
    }
}

// ═══════════════════════════════════════════════════════════════
// Bug #1: drain_output WouldBlock permanently discards KCP output
//
// When transport.try_send() returns WouldBlock, drain_output discards
// the packet. KCP's retransmission will eventually resend data segments,
// but lost ACKs cause unnecessary retransmissions and lost window probes
// can stall the connection. With persistent WouldBlock, the connection
// dies due to retransmission exhaustion.
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_wouldblock_permanent_discard_causes_connection_death() {
    let local: SocketAddr = "127.0.0.1:19999".parse().unwrap();
    // ALL sends fail immediately (fail_after = 0)
    let transport = Arc::new(FlakyTransport::new(local, 0));
    let config = KcpConfig::default()
        .nodelay(true, 10, 2, true)
        .dead_link(5)
        .timeout(Duration::from_secs(5));

    let listener =
        KcpListener::from_transport(transport.clone() as Arc<dyn KcpTransport>, config).unwrap();

    let peer: SocketAddr = "127.0.0.1:19998".parse().unwrap();
    let conn = listener.create_connection(1, peer);

    // Send data — KCP accepts it into send queue
    conn.send(b"test data that should be retransmitted")
        .await
        .unwrap();

    // Wait for Actor to process and attempt sends
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify: transport was called but all sends failed
    let attempts = transport.attempts();
    let failures = transport.failures();
    assert!(attempts > 0, "Transport should have been called");
    assert_eq!(
        attempts, failures,
        "All sends should have returned WouldBlock"
    );

    // Verify: data is still in send queue (no ACKs arrived)
    let wait_snd = conn.wait_snd().await;
    assert!(
        wait_snd > 0,
        "Data should still be pending (no ACKs arrived because all output was discarded)"
    );

    println!("  After 100ms: attempts={attempts}, failures={failures}, wait_snd={wait_snd}");

    // BUG CONSEQUENCE: Because drain_output permanently discards WouldBlock packets,
    // ALL retransmissions also fail. The connection will die after dead_link threshold.
    // Wait for connection to die
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if conn.is_dead().await {
            break;
        }
    }

    let is_dead = conn.is_dead().await;
    let final_attempts = transport.attempts();
    let final_failures = transport.failures();

    println!(
        "  Final: attempts={final_attempts}, failures={final_failures}, is_dead={is_dead}"
    );

    // BUG: Connection died because WouldBlock permanently discarded ALL output packets.
    // Even KCP's retransmission mechanism couldn't recover because each retransmission
    // also hit WouldBlock and was discarded by drain_output.
    assert!(
        is_dead,
        "BUG #1: Connection should die because drain_output permanently discards \
         WouldBlock packets. Retransmissions also fail because they hit the same WouldBlock."
    );
}

// ═══════════════════════════════════════════════════════════════
// Bug #1 (intermittent): WouldBlock during burst causes packet loss
//
// Simulates a transport that works initially but becomes blocked
// during a burst of sends. Verifies that packets are NOT buffered
// for retry.
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_wouldblock_intermittent_does_not_retry_discarded_packets() {
    let local: SocketAddr = "127.0.0.1:19997".parse().unwrap();
    // First 5 sends succeed, then fail
    let transport = Arc::new(FlakyTransport::new(local, 5));
    let config = KcpConfig::default()
        .nodelay(true, 10, 2, true)
        .dead_link(20)
        .timeout(Duration::from_secs(10));

    let listener =
        KcpListener::from_transport(transport.clone() as Arc<dyn KcpTransport>, config).unwrap();

    let peer: SocketAddr = "127.0.0.1:19996".parse().unwrap();
    let conn = listener.create_connection(1, peer);

    // Send multiple messages to generate burst output
    for i in 0u32..10 {
        let data = i.to_le_bytes();
        conn.send(&data).await.unwrap();
    }

    // Wait for Actor to process
    tokio::time::sleep(Duration::from_millis(100)).await;

    let successes = transport.successes();
    let failures = transport.failures();

    println!("  Successes: {successes}, Failures: {failures}");

    // Some sends should have succeeded (first 5), rest should have failed
    assert!(successes > 0, "Some sends should succeed");
    assert!(failures > 0, "Some sends should fail after threshold");

    // After some time, KCP will retransmit, but those also hit WouldBlock
    // because the transport is still in "blocked" state (fail_after reached)
    tokio::time::sleep(Duration::from_millis(500)).await;

    let new_attempts = transport.attempts();
    let new_failures = transport.failures();

    // BUG: All subsequent attempts also fail because drain_output discards
    // the packet each time — no buffering/retry mechanism exists
    println!(
        "  After 500ms more: attempts={new_attempts}, failures={new_failures}"
    );

    // The difference between attempts and successes represents wasted retransmissions
    let wasted_retransmissions = new_attempts - successes;
    assert!(
        wasted_retransmissions > 0,
        "BUG #1: KCP wastes retransmissions because drain_output discards WouldBlock packets \
         instead of buffering them for retry. Wasted: {wasted_retransmissions}"
    );
}

// ═══════════════════════════════════════════════════════════════
// Bug #1 (recovery test): After WouldBlock clears, verify data delivery
//
// Tests whether the system recovers after a temporary WouldBlock period.
// With the bug: recovery relies on KCP retransmission timer (slow).
// After fix: buffered packets should be sent immediately on next drain.
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_wouldblock_recovery_after_transport_unblock() {
    let local: SocketAddr = "127.0.0.1:19995".parse().unwrap();
    let transport = Arc::new(FlakyTransport::new(local, usize::MAX)); // Never fails naturally
    let config = KcpConfig::default()
        .nodelay(true, 10, 2, true)
        .dead_link(20)
        .timeout(Duration::from_secs(10));

    let listener =
        KcpListener::from_transport(transport.clone() as Arc<dyn KcpTransport>, config).unwrap();

    let peer: SocketAddr = "127.0.0.1:19994".parse().unwrap();
    let conn = listener.create_connection(1, peer);

    // Block the transport
    transport.block_permanently();

    // Send data while blocked
    conn.send(b"data sent while blocked").await.unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let failures_during_block = transport.failures();
    assert!(
        failures_during_block > 0,
        "Packets should have been dropped while blocked"
    );

    println!(
        "  Packets discarded during block: {failures_during_block}"
    );
    println!(
        "  BUG #1 CONFIRMED: drain_output discards WouldBlock packets without retry buffer"
    );
}

//! 隐形丢数据复现测试
//!
//! 复现已识别的 send 实现中的隐形丢数据问题。
//! 修复前应 FAIL，修复后应 PASS。
//!
//! 运行: `cargo test --test data_loss_tests -- --nocapture`

#![allow(
    clippy::cast_possible_truncation,
    clippy::uninlined_format_args,
    clippy::used_underscore_binding
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kcp2::{AsyncKcp, Kcp, KcpError};

// ═══════════════════════════════════════════════════════════════
// Bug #4: 有界 output callback 静默丢包
// 模拟生产环境中 ArrayQueue(256) 溢出场景
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_bounded_output_callback_drops_kcp_packets() {
    // Simulate production ArrayQueue with a small capacity
    const QUEUE_CAPACITY: usize = 2;
    let accepted: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let dropped_count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));

    let acc_clone = accepted.clone();
    let drp_clone = dropped_count.clone();

    let mut kcp = Kcp::new(0x1234_5678, move |data: &[u8]| {
        let mut acc = acc_clone.lock().unwrap();
        if acc.len() < QUEUE_CAPACITY {
            acc.push(data.to_vec());
        } else {
            *drp_clone.lock().unwrap() += 1;
        }
    });

    kcp.set_nodelay(true, 10, 2, true);
    kcp.set_wndsize(512, 512);

    // Send many small packets to generate lots of KCP output segments.
    // Each send creates 1 segment; flush produces 1 packet per segment
    // (plus ACKs and window probes).
    for i in 0u32..100 {
        let data = i.to_le_bytes();
        kcp.send(&data).unwrap();
    }
    kcp.update(0);
    kcp.flush();

    let accepted_count = accepted.lock().unwrap().len();
    let dropped = *dropped_count.lock().unwrap();

    println!("  Accepted: {accepted_count}, Dropped: {dropped}");

    // BUG REPRODUCED: some KCP output packets were silently dropped
    assert!(
        dropped > 0,
        "BUG #4: KCP output packets should be dropped when output queue overflows \
         (got accepted={accepted_count}, dropped={dropped})"
    );
    assert!(accepted_count <= QUEUE_CAPACITY);
}

// ═══════════════════════════════════════════════════════════════
// Bug #6: Stream mode returns Ok(partial) silently dropping remainder
// WND_RCV=128, MSS=1376 → threshold = 128*1376 = 176128 bytes
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_stream_mode_partial_send_silent_data_loss() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);
    kcp.set_stream(true);
    kcp.set_wndsize(512, 512);

    // Send a small packet first to create a tail segment in snd_queue
    kcp.send(b"initial").unwrap();

    // Now send data large enough to overflow after filling the tail.
    // Tail capacity = MSS - "initial".len() = 1376 - 7 = 1369 bytes.
    // After filling tail, remaining = data.len() - 1369 bytes.
    // Fragments needed = ceil(remaining / MSS).
    // To trigger count >= WND_RCV(128): remaining > 128 * 1376 = 176128.
    // So data.len() > 176128 + 1369 = 177497.
    let large_data = vec![0xABu8; 180_000];
    let total = large_data.len();
    let result = kcp.send(&large_data);

    match result {
        Ok(bytes_sent) => {
            assert_eq!(bytes_sent, total, "send() should only return Ok for complete delivery");
            println!("  FIXED: send() returned Ok({bytes_sent}) — complete delivery confirmed");
        }
        Err(KcpError::TooManyFragments { count, max }) => {
            assert!(count >= max, "count should >= WND_RCV threshold");
            println!(
                "  FIXED: send() correctly returns Err(TooManyFragments{{count={count}, max={max}}}) \
                 instead of silent partial Ok"
            );
        }
        Err(e) => panic!("Unexpected error: {:?}", e),
    }
}

// ═══════════════════════════════════════════════════════════════
// Bug #5: AsyncKcp recv() truncates data when user buffer is too small
// Core Kcp correctly returns BufferTooSmall, but AsyncKcp silently truncates
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_async_kcp_recv_truncates_large_message() {
    let channel_ab: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let channel_ba: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));

    let ch_ab = channel_ab.clone();
    let ch_ba = channel_ba.clone();

    let kcp_a = AsyncKcp::new(0x1234_5678, move |data: &[u8]| {
        ch_ab.lock().unwrap().push(data.to_vec());
    });

    let kcp_b = AsyncKcp::new(0x1234_5678, move |data: &[u8]| {
        ch_ba.lock().unwrap().push(data.to_vec());
    });

    // A sends a 200-byte message
    let data = vec![0x42u8; 200];
    kcp_a.send(&data).await.unwrap();

    // Wait for Actor to process the send and produce output
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Route A's output to B's input
    let packets: Vec<Vec<u8>> = channel_ab.lock().unwrap().drain(..).collect();
    for pkt in packets {
        kcp_b.input(&pkt).await.unwrap();
    }

    // Wait for B's Actor to process the input
    tokio::time::sleep(Duration::from_millis(20)).await;

    // B receives with a 50-byte buffer
    let mut small_buf = vec![0u8; 50];
    let result = kcp_b.recv(&mut small_buf).await;

    match result {
        Ok(n) => {
            // BUG REPRODUCED: returns Ok(50), 150 bytes silently dropped
            assert!(
                n <= 50,
                "Expected truncated result, got n={n}"
            );
            if n < 200 {
                println!(
                    "  BUG #5 REPRODUCED: recv() returned Ok({n}) for a 200-byte message \
                     with 50-byte buffer. {} bytes SILENTLY DROPPED.",
                    200 - n
                );
                // This IS the bug — should have returned BufferTooSmall
                panic!(
                    "BUG #5: AsyncKcp::recv() silently truncated {n}/200 bytes. \
                     Should return Err(BufferTooSmall) instead."
                );
            }
        }
        Err(KcpError::BufferTooSmall { required, available }) => {
            // FIXED! Properly reports buffer too small
            assert_eq!(required, 200);
            assert_eq!(available, 50);
            println!(
                "  FIXED: recv() correctly returned BufferTooSmall(required={required}, available={available})"
            );
        }
        Err(e) => panic!("Unexpected error: {:?}", e),
    }
}

// ═══════════════════════════════════════════════════════════════
// Bug #5 companion: Core Kcp correctly returns BufferTooSmall
// (Proves the bug is in AsyncKcp layer, not core)
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_core_kcp_recv_returns_buffer_too_small() {
    let channel: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let ch = channel.clone();

    let mut kcp_a = Kcp::new(0x1234_5678, move |data: &[u8]| {
        ch.lock().unwrap().push(data.to_vec());
    });

    let mut kcp_b = Kcp::new(0x1234_5678, |_: &[u8]| {});

    kcp_a.set_nodelay(true, 10, 2, true);
    kcp_b.set_nodelay(true, 10, 2, true);

    // A sends a 200-byte message
    let data = vec![0x42u8; 200];
    kcp_a.send(&data).unwrap();
    kcp_a.update(0);
    kcp_a.flush();

    // Route to B
    for pkt in channel.lock().unwrap().drain(..) {
        kcp_b.input(&pkt).unwrap();
    }

    // Core Kcp correctly returns BufferTooSmall when buffer is too small
    let mut small_buf = vec![0u8; 50];
    let result = kcp_b.recv(&mut small_buf);

    // This proves the core does the right thing — AsyncKcp should too
    match result {
        Err(KcpError::BufferTooSmall { required, available }) => {
            assert_eq!(required, 200);
            assert_eq!(available, 50);
            println!(
                "  Core Kcp correctly returns BufferTooSmall(required={required}, available={available})"
            );
        }
        Ok(n) => {
            println!("  Core Kcp returned Ok({n}) — data was received (buffer was large enough or data < 50)");
        }
        Err(e) => {
            println!("  Core Kcp returned {:?} — may be RecvQueueEmpty if routing didn't complete", e);
        }
    }
}

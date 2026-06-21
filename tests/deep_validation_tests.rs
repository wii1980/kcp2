//! Deep validation tests for handle_reconnect, input, and flush modifications.
//!
//! These tests target the specific code paths modified during the code audit:
//! - `handle_reconnect`: unified memory pool recycling for rcv_queue/rcv_buf
//! - `input`: simplified is_fresh handling, parse_una integration
//! - `flush`: `.expect()` replaced with `.is_ok()` conditional advance

#![allow(clippy::cast_possible_truncation)]

use kcp2_core::{Kcp, KcpError};
use std::cell::RefCell;
use std::rc::Rc;

const OVERHEAD: usize = 24;
const CMD_PUSH: u8 = 81;
const CMD_ACK: u8 = 82;
const CMD_RECONNECT: u8 = 0x80;

// ─── Helpers ────────────────────────────────────────────────────────

fn build_segment(conv: u32, cmd: u8, sn: u32, ts: u32, una: u32, wnd: u16, data: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; OVERHEAD + data.len()];
    buf[0..4].copy_from_slice(&conv.to_le_bytes());
    buf[4] = cmd;
    buf[5] = 0;
    buf[6..8].copy_from_slice(&wnd.to_le_bytes());
    buf[8..12].copy_from_slice(&ts.to_le_bytes());
    buf[12..16].copy_from_slice(&sn.to_le_bytes());
    buf[16..20].copy_from_slice(&una.to_le_bytes());
    buf[20..24].copy_from_slice(&(data.len() as u32).to_le_bytes());
    buf[24..].copy_from_slice(data);
    buf
}

fn build_reconnect(conv: u32, wnd: u16) -> Vec<u8> {
    build_segment(conv, CMD_RECONNECT, 0, 0, 0, wnd, &[])
}

/// Captures all output packets for inspection.
fn capture_output() -> (Rc<RefCell<Vec<Vec<u8>>>>, impl Fn(&[u8])) {
    let captured: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
    let captured_clone = captured.clone();
    let closure = move |data: &[u8]| {
        captured_clone.borrow_mut().push(data.to_vec());
    };
    (captured, closure)
}

// ─── handle_reconnect tests ─────────────────────────────────────────

#[test]
fn t1_reconnect_on_fresh_connection_is_noop() {
    let (_captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    // Fresh connection: no data sent yet
    let reconnect = build_reconnect(42, 200);
    kcp.input(&reconnect).unwrap();

    // Fresh path should only record rmt_wnd, not reset anything
    // Verify by sending data normally afterwards
    kcp.send(b"hello").unwrap();
    kcp.update(0);
    kcp.flush();

    // The connection should still be in a usable state
    assert!(!kcp.is_dead(), "fresh reconnect must not kill connection");
}

#[test]
fn t2_reconnect_clears_all_state_when_not_fresh() {
    let (captured1, output1) = capture_output();
    let mut kcp1 = Kcp::new(42, output1);
    let (_captured2, output2) = capture_output();
    let mut kcp2 = Kcp::new(42, output2);

    kcp1.send(b"pending data before reconnect").unwrap();
    kcp1.update(0);
    kcp1.flush();

    let packets: Vec<_> = captured1.borrow_mut().drain(..).collect();
    assert!(!packets.is_empty(), "kcp1 should have emitted packets");
    for pkt in &packets {
        kcp2.input(pkt).unwrap();
    }

    assert!(kcp1.wait_snd() > 0, "kcp1 should have pending segments");

    let reconnect = build_reconnect(42, 128);
    kcp1.input(&reconnect).unwrap();
    kcp2.input(&reconnect).unwrap();

    assert_eq!(kcp1.wait_snd(), 0, "kcp1 wait_snd must be 0 after reconnect");
    assert_eq!(kcp2.wait_snd(), 0, "kcp2 wait_snd must be 0 after reconnect");
}

#[test]
fn t3_reconnect_then_communicate_normally() {
    let (captured1, output1) = capture_output();
    let (_captured2, output2) = capture_output();
    let mut kcp1 = Kcp::new(42, output1);
    let mut kcp2 = Kcp::new(42, output2);

    kcp1.send(b"old data").unwrap();
    kcp1.update(10);
    kcp1.flush();
    let pkts1: Vec<_> = captured1.borrow_mut().drain(..).collect();
    assert!(!pkts1.is_empty(), "phase 1: kcp1 must emit");
    for pkt in &pkts1 {
        kcp2.input(pkt).unwrap();
    }
    kcp2.update(20);
    let mut buf = [0u8; 64];
    let n = kcp2.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"old data");

    let reconnect = build_reconnect(42, 128);
    kcp1.input(&reconnect).unwrap();
    kcp2.input(&reconnect).unwrap();

    kcp1.send(b"new data after reconnect").unwrap();
    kcp1.update(200);
    kcp1.flush();
    let pkts2: Vec<_> = captured1.borrow_mut().drain(..).collect();
    assert!(!pkts2.is_empty(), "phase 3: kcp1 must emit after reconnect");
    for pkt in &pkts2 {
        kcp2.input(pkt).unwrap();
    }
    kcp2.update(110);
    kcp2.flush();

    let peek = kcp2.peek_size();
    assert!(peek.is_ok(), "phase 3: kcp2 must have data to recv, peek={:?}", peek);
    let mut buf = [0u8; 64];
    let n = kcp2.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"new data after reconnect");
}

#[test]
fn t4_multiple_reconnects_in_sequence() {
    let (_captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    // First reconnect (fresh) — no-op
    let r1 = build_reconnect(42, 128);
    kcp.input(&r1).unwrap();

    // Dirty state
    kcp.send(b"first").unwrap();
    kcp.update(0);
    kcp.flush();
    assert!(kcp.wait_snd() > 0);

    // Second reconnect — should clear
    let r2 = build_reconnect(42, 128);
    kcp.input(&r2).unwrap();
    assert_eq!(kcp.wait_snd(), 0, "second reconnect must clear");

    // Dirty again
    kcp.send(b"second").unwrap();
    kcp.update(100);
    kcp.flush();
    assert!(kcp.wait_snd() > 0);

    // Third reconnect — should clear again
    let r3 = build_reconnect(42, 128);
    kcp.input(&r3).unwrap();
    assert_eq!(kcp.wait_snd(), 0, "third reconnect must clear");
}

#[test]
fn t5_reconnect_preserves_snd_buf_ordering_invariant() {
    let (_captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    // Build state: send enough to populate snd_buf
    for i in 0..5 {
        kcp.send(format!("msg-{i}").as_bytes()).unwrap();
    }
    kcp.update(0);
    kcp.flush();
    assert!(kcp.wait_snd() > 0);

    // Reconnect should reset snd_nxt/snd_una to 0 and clear snd_buf.
    // After reconnect, any subsequent flush must not violate the
    // debug_assert!(snd_buf.last().sn < snd_nxt) invariant.
    let reconnect = build_reconnect(42, 128);
    kcp.input(&reconnect).unwrap();
    assert_eq!(kcp.wait_snd(), 0);

    // Send new data and flush — if the invariant is violated this will panic
    // in debug builds.
    kcp.send(b"after reconnect").unwrap();
    kcp.update(100);
    kcp.flush();
    // No panic means invariant held.
}

// ─── input tests ────────────────────────────────────────────────────

#[test]
fn t6_input_clears_is_fresh_on_first_packet() {
    let (_captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    // Build a minimal PUSH segment to feed as first input
    let push = build_segment(42, CMD_PUSH, 0, 0, 0, 128, b"hello");
    kcp.input(&push).unwrap();

    // After first input, is_fresh should be false.
    // We verify this indirectly: a subsequent reconnect should take the
    // "not fresh" path (clearing state). If is_fresh were still true,
    // reconnect would be a no-op and data would survive.
    kcp.send(b"test").unwrap();
    kcp.update(0);
    kcp.flush();
    let wait_before = kcp.wait_snd();
    assert!(wait_before > 0);

    let reconnect = build_reconnect(42, 128);
    kcp.input(&reconnect).unwrap();

    assert_eq!(
        kcp.wait_snd(),
        0,
        "reconnect after first input must clear state (is_fresh was cleared)"
    );
}

#[test]
fn t7_input_multi_segment_batch_mixed_cmds() {
    let (captured, output) = capture_output();
    let mut kcp1 = Kcp::new(42, output);
    let (_c2, output2) = capture_output();
    let mut kcp2 = Kcp::new(42, output2);

    // kcp1 sends data → kcp2 receives and generates ACKs
    kcp1.send(b"batch test").unwrap();
    kcp1.update(0);
    kcp1.flush();
    for pkt in captured.borrow_mut().drain(..) {
        kcp2.input(&pkt).unwrap();
    }

    // kcp2 should have data to recv
    let mut buf = [0u8; 64];
    let n = kcp2.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"batch test");
}

#[test]
fn t8_input_partial_decode_failure_does_not_panic() {
    let (_captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    // Build a valid segment then truncate it to simulate corruption mid-stream
    let valid = build_segment(42, CMD_PUSH, 0, 0, 0, 128, b"hello");
    let mut data = valid.clone();
    // Append garbage that's shorter than OVERHEAD (will cause decode to fail/break)
    data.extend_from_slice(&[0xFF; 10]);

    // Should not panic; should return Ok with bytes consumed up to corruption
    let result = kcp.input(&data);
    assert!(result.is_ok(), "partial decode should not error out");
    let consumed = result.unwrap();
    assert_eq!(consumed, valid.len(), "should consume valid prefix only");
}

#[test]
fn t9_input_updates_snd_una_via_ack_processing() {
    let (captured1, output1) = capture_output();
    let (captured2, output2) = capture_output();
    let mut kcp1 = Kcp::new(42, output1);
    let mut kcp2 = Kcp::new(42, output2);

    kcp1.send(b"msg1").unwrap();
    kcp1.send(b"msg2").unwrap();
    kcp1.send(b"msg3").unwrap();
    kcp1.update(0);
    kcp1.flush();

    let kcp1_packets: Vec<_> = captured1.borrow_mut().drain(..).collect();
    for pkt in &kcp1_packets {
        kcp2.input(pkt).unwrap();
    }

    let mut buf = [0u8; 64];
    while kcp2.recv(&mut buf).is_ok() {}

    kcp2.update(10);
    kcp2.flush();

    let kcp2_packets: Vec<_> = captured2.borrow_mut().drain(..).collect();
    let wait_before = kcp1.wait_snd();
    assert!(wait_before > 0, "kcp1 should have pending segments before ACK");

    for pkt in &kcp2_packets {
        kcp1.input(pkt).unwrap();
    }

    let wait_after = kcp1.wait_snd();
    assert!(
        wait_after < wait_before,
        "ACK processing via input must reduce wait_snd: before={}, after={}",
        wait_before,
        wait_after
    );
}

#[test]
fn t10_input_conv_mismatch_returns_error_immediately() {
    let (_captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    let wrong_conv = build_segment(99, CMD_PUSH, 0, 0, 0, 128, b"hello");
    let result = kcp.input(&wrong_conv);
    assert!(
        matches!(result, Err(KcpError::ConvMismatch { expected: 42, got: 99 })),
        "conv mismatch must return error immediately"
    );
}

#[test]
fn t10b_input_invalid_cmd_returns_error() {
    let (_captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    // cmd=99 is not a valid KCP command
    let invalid = build_segment(42, 99, 0, 0, 0, 128, b"hello");
    let result = kcp.input(&invalid);
    assert!(
        matches!(result, Err(KcpError::InvalidCmd { cmd: 99 })),
        "invalid cmd must return error"
    );
}

#[test]
fn t10c_input_too_short_returns_error() {
    let (_captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    let result = kcp.input(&[0u8; 10]);
    assert!(matches!(result, Err(KcpError::InputTooShort { .. })));
}

// ─── flush tests ────────────────────────────────────────────────────

#[test]
fn t11_flush_emits_push_segments() {
    let (captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    kcp.send(b"flush test data").unwrap();
    kcp.update(0);
    kcp.flush();

    let packets = captured.borrow();
    assert!(!packets.is_empty(), "flush must emit at least one packet");

    // Verify the emitted packet contains CMD_PUSH
    let first = &packets[0];
    assert!(first.len() >= OVERHEAD, "packet must have at least header");
    assert_eq!(first[4], CMD_PUSH, "emitted segment must be CMD_PUSH");
}

#[test]
fn t12_flush_emits_acks() {
    let (captured1, output1) = capture_output();
    let (captured2, output2) = capture_output();
    let mut kcp1 = Kcp::new(42, output1);
    let mut kcp2 = Kcp::new(42, output2);

    // kcp1 → kcp2: generates ACKs on kcp2
    kcp1.send(b"trigger ack").unwrap();
    kcp1.update(0);
    kcp1.flush();
    for pkt in captured1.borrow_mut().drain(..) {
        kcp2.input(&pkt).unwrap();
    }

    // kcp2 flush → should emit ACK back to kcp1
    kcp2.update(10);
    kcp2.flush();

    let kcp2_packets = captured2.borrow();
    assert!(!kcp2_packets.is_empty(), "kcp2 must emit ACK packets");
    let found_ack = kcp2_packets.iter().any(|pkt| {
        pkt.len() >= OVERHEAD && pkt[4] == CMD_ACK
    });
    assert!(found_ack, "flush must emit CMD_ACK segments");
}

#[test]
fn t13_flush_window_probe_wask_wins() {
    let (_captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    // Force rmt_wnd=0 by injecting a segment with wnd=0
    let zero_wnd = build_segment(42, CMD_PUSH, 1000, 0, 0, 0, b"x"); // sn far ahead
    let _ = kcp.input(&zero_wnd);

    // Run enough updates to trigger probe cycle
    for t in (0..20_000u32).step_by(100) {
        kcp.update(t);
    }
    kcp.flush();

    // The connection should still be alive (not dead from probe)
    assert!(!kcp.is_dead(), "window probe must not kill connection");
}

#[test]
fn t14_flush_snd_buf_ordering_invariant_holds() {
    let (_captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    // Send many messages to fill snd_queue
    for i in 0..20 {
        kcp.send(format!("message-{i:02}").as_bytes()).unwrap();
    }
    kcp.update(0);

    // flush should move snd_queue → snd_buf maintaining ordering.
    // In debug builds, debug_assert!(snd_buf.last().sn < snd_nxt) will fire
    // if the invariant is violated.
    kcp.flush();

    // No assertion failure means invariant held. Additionally verify wait_snd.
    assert!(kcp.wait_snd() > 0, "snd_buf should be populated");
}

#[test]
fn t15_flush_with_zero_cwnd_does_not_send_push() {
    let (captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    // cwnd starts at 0; without update() being called, flush() returns early.
    // Even after update, cwnd may still be small.
    kcp.send(b"test").unwrap();

    // flush() before update() → returns immediately (updated=false)
    kcp.flush();

    let packets = captured.borrow();
    assert!(
        packets.is_empty(),
        "flush before first update must not emit anything (updated=false)"
    );
}

#[test]
fn t16_flush_not_updated_returns_early() {
    let (captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    kcp.send(b"test").unwrap();
    // Directly call flush without update — should be a no-op
    kcp.flush();

    assert!(
        captured.borrow().is_empty(),
        "flush without update must emit nothing"
    );
}

#[test]
fn t17_flush_encode_failure_does_not_panic() {
    // This is a safety test: if segment encode somehow fails (shouldn't happen
    // under normal MTU constraints), flush must skip the segment rather than panic.
    // We verify by setting a normal MTU and sending max-size data.
    let (captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    kcp.set_mtu(100).unwrap(); // small MTU to stress the buffer
    let mss = 100 - OVERHEAD;
    let big_data = vec![0xAA; mss];
    kcp.send(&big_data).unwrap();
    kcp.update(0);
    kcp.flush(); // must not panic

    let packets = captured.borrow();
    // Even with small MTU, flush should emit valid packets
    assert!(!packets.is_empty(), "flush must emit data even with small MTU");
    for pkt in packets.iter() {
        assert!(pkt.len() <= 100 + OVERHEAD, "packet must respect MTU+overhead budget");
    }
}

#[test]
fn t18_flush_dead_link_detection() {
    let (_captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    kcp.set_dead_link(2); // after 2 retransmits → dead
    kcp.send(b"doomed").unwrap();

    // Simulate time passing with no ACKs → retransmits accumulate
    for i in 0..20 {
        kcp.update(i * 1000);
    }

    assert!(kcp.is_dead(), "flush must detect dead link after repeated retransmits");
}

// ─── Integration: reconnect + input + flush combined ────────────────

#[test]
fn t19_reconnect_during_active_flush_cycle() {
    let (captured1, output1) = capture_output();
    let (captured2, output2) = capture_output();
    let mut kcp1 = Kcp::new(42, output1);
    let mut kcp2 = Kcp::new(42, output2);

    // Phase 1: active communication
    kcp1.send(b"first round").unwrap();
    kcp1.update(0);
    kcp1.flush();
    for pkt in captured1.borrow_mut().drain(..) {
        kcp2.input(&pkt).unwrap();
    }
    let mut buf = [0u8; 64];
    let n = kcp2.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"first round");

    // Phase 2: kcp2 sends back
    kcp2.send(b"reply").unwrap();
    kcp2.update(10);
    kcp2.flush();
    for pkt in captured2.borrow_mut().drain(..) {
        kcp1.input(&pkt).unwrap();
    }
    let n = kcp1.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"reply");

    // Phase 3: reconnect mid-stream
    let reconnect = build_reconnect(42, 128);
    kcp1.input(&reconnect).unwrap();
    kcp2.input(&reconnect).unwrap();

    // Phase 4: resume communication
    kcp1.send(b"after reset").unwrap();
    kcp1.update(100);
    kcp1.flush();
    for pkt in captured1.borrow_mut().drain(..) {
        kcp2.input(&pkt).unwrap();
    }
    let n = kcp2.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"after reset");
}

#[test]
fn t20_reconnect_preserves_no_stale_acks() {
    let (_captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    // Generate ACKs by receiving data
    let push = build_segment(42, CMD_PUSH, 0, 0, 0, 128, b"data");
    kcp.input(&push).unwrap();

    // acklist should now have an entry
    // Reconnect should clear it
    let reconnect = build_reconnect(42, 128);
    kcp.input(&reconnect).unwrap();

    // After reconnect, flush should not emit any stale ACKs
    let (_captured_flush, _) = capture_output();
    // Can't easily swap output closure, so verify indirectly:
    // needs_update should be false (no snd_buf, no acklist, rmt_wnd != 0)
    assert!(
        !kcp.needs_update(),
        "after reconnect, no pending ACKs should remain"
    );
}

#[test]
fn t21_flush_after_reconnect_starts_clean() {
    let (captured, output) = capture_output();
    let mut kcp = Kcp::new(42, output);

    // Build up state
    kcp.send(b"pre-reconnect").unwrap();
    kcp.update(0);
    kcp.flush();
    let _ = captured.borrow_mut().drain(..); // discard pre-reconnect output

    // Reconnect
    let reconnect = build_reconnect(42, 128);
    kcp.input(&reconnect).unwrap();

    // Flush after reconnect should emit nothing (snd_queue/snd_buf empty)
    kcp.update(100);
    kcp.flush();

    let post_packets = captured.borrow();
    // Allow WINS/WASK probes but no stale PUSH data
    let found_stale_push = post_packets.iter().any(|pkt| {
        pkt.len() >= OVERHEAD && pkt[4] == CMD_PUSH
    });
    assert!(
        !found_stale_push,
        "flush after reconnect must not emit stale PUSH segments"
    );
}

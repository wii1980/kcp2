use std::cell::RefCell;
use std::rc::Rc;

use kcp2::{AsyncKcp, Kcp, KcpError, Segment};

struct LoopbackChannel {
    packets: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl LoopbackChannel {
    fn new() -> Self {
        Self {
            packets: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn clone(&self) -> Self {
        Self {
            packets: self.packets.clone(),
        }
    }

    fn drain(&self) -> Vec<Vec<u8>> {
        self.packets.borrow_mut().drain(..).collect()
    }

    fn push(&self, data: &[u8]) {
        self.packets.borrow_mut().push(data.to_vec());
    }
}

#[allow(clippy::type_complexity)]
fn create_loopback_pair(
    conv: u32,
) -> (
    Kcp<impl Fn(&[u8])>,
    Kcp<impl Fn(&[u8])>,
    LoopbackChannel,
    LoopbackChannel,
) {
    let channel1 = LoopbackChannel::new();
    let channel2 = LoopbackChannel::new();

    let ch1_clone = channel1.clone();
    let ch2_clone = channel2.clone();

    let kcp1 = Kcp::new(conv, move |data: &[u8]| {
        ch2_clone.push(data);
    });

    let kcp2 = Kcp::new(conv, move |data: &[u8]| {
        ch1_clone.push(data);
    });

    (kcp1, kcp2, channel1, channel2)
}

fn simulate_tick(kcp: &mut Kcp<impl Fn(&[u8])>, time: u32) {
    kcp.update(time);
    kcp.flush();
}

#[test]
fn test_kcp_basic() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1122_3344, output);

    let data = b"hello kcp";
    kcp.send(data).unwrap();

    assert!(kcp.wait_snd() > 0);
}

#[test]
fn test_kcp_loopback() {
    let buf1: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
    let buf2: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));

    let buf1_clone = buf1.clone();
    let buf2_clone = buf2.clone();

    let mut kcp1 = Kcp::new(0x1122_3344, move |data: &[u8]| {
        buf2_clone.borrow_mut().push(data.to_vec());
    });

    let mut kcp2 = Kcp::new(0x1122_3344, move |data: &[u8]| {
        buf1_clone.borrow_mut().push(data.to_vec());
    });

    kcp1.send(b"hello from kcp1").unwrap();
    kcp1.update(0);
    kcp1.flush();

    for pkt in buf2.borrow_mut().drain(..) {
        kcp2.input(&pkt).unwrap();
    }

    let mut recv_buf = vec![0u8; 1024];
    let size = kcp2.recv(&mut recv_buf).unwrap();
    assert_eq!(&recv_buf[..size], b"hello from kcp1");
}

#[test]
fn test_send_with_handle_stream_mode() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);
    kcp.set_stream(true);

    let handle1 = kcp.send_with_handle(b"hello").unwrap();
    assert_eq!(handle1.sn_start, 0);
    assert_eq!(handle1.sn_end, 0);

    let handle2 = kcp.send_with_handle(b" world").unwrap();
    assert!(handle2.sn_start <= 1);
    assert!(handle2.sn_end <= 1);
}

#[test]
fn test_send_with_handle_non_stream_mode() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);
    kcp.set_stream(false);

    let handle1 = kcp.send_with_handle(b"packet1").unwrap();
    assert_eq!(handle1.sn_start, 0);
    assert_eq!(handle1.sn_end, 0);

    let handle2 = kcp.send_with_handle(b"packet2").unwrap();
    assert_eq!(handle2.sn_start, 1);
    assert_eq!(handle2.sn_end, 1);
}

#[test]
fn test_send_with_handle_empty_data() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    let result = kcp.send_with_handle(b"");
    assert!(matches!(result, Err(KcpError::EmptyData)));
}

#[test]
fn test_is_send_acked() {
    let buf: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
    let buf_clone = buf.clone();

    let mut kcp = Kcp::new(0x1234_5678, move |data: &[u8]| {
        buf_clone.borrow_mut().push(data.to_vec());
    });

    let handle = kcp.send_with_handle(b"test message").unwrap();

    assert!(!kcp.is_send_acked(handle));

    kcp.update(0);
    kcp.flush();

    assert!(!kcp.is_send_acked(handle));
}

#[tokio::test]
async fn test_async_kcp_no_deadlock_in_output_callback() {
    use std::sync::Arc;
    use std::sync::Mutex;

    let callback_called = Arc::new(Mutex::new(false));
    let callback_called_clone = callback_called.clone();

    let output = move |_data: &[u8]| {
        *callback_called_clone.lock().unwrap() = true;
    };

    let kcp = AsyncKcp::new(0x1234_5678, output);

    let result = kcp.send(b"test").await;
    assert!(result.is_ok());

    for _ in 0..2000 {
        if *callback_called.lock().unwrap() {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
    }

    let called = *callback_called.lock().unwrap();
    assert!(called, "Output callback should have been called");
}

#[test]
fn test_current_monotonic() {
    let time1 = kcp2::current();
    std::thread::sleep(std::time::Duration::from_millis(10));
    let time2 = kcp2::current();
    assert!(
        time2 >= time1,
        "current() should be monotonic: {time2} >= {time1}"
    );
}

#[test]
fn test_current_no_large_values() {
    let time = kcp2::current();
    assert!(
        time < 3_600_000,
        "current() should return relative time: {time} < 3_600_000"
    );
}

#[test]
fn test_fastack_conserve_feature() {
    let (mut kcp1, mut kcp2, channel1, channel2) = create_loopback_pair(0x1234_5678);

    kcp1.send(b"packet1").unwrap();
    kcp1.send(b"packet2").unwrap();
    kcp1.send(b"packet3").unwrap();

    simulate_tick(&mut kcp1, 0);

    let packets = channel2.drain();
    assert!(!packets.is_empty(), "should have sent data packets");
    for packet in &packets {
        kcp2.input(packet).unwrap();
    }

    simulate_tick(&mut kcp2, 10);

    let acks = channel1.drain();
    assert!(!acks.is_empty(), "should have ACK packets");
    for ack in &acks {
        kcp1.input(ack).unwrap();
    }
}

#[test]
fn test_retransmission() {
    let (mut kcp1, mut _kcp2, _channel1, channel2) = create_loopback_pair(0x1234_5678);

    kcp1.send(b"test data").unwrap();

    simulate_tick(&mut kcp1, 0);
    let packets1 = channel2.drain();
    assert!(!packets1.is_empty(), "Data should be sent on first tick");

    simulate_tick(&mut kcp1, 500);

    let packets2 = channel2.drain();
    assert!(
        !packets2.is_empty(),
        "Data should be retransmitted after RTO"
    );
}

#[test]
fn test_dead_link_detection() {
    let (mut kcp1, _kcp2, _channel1, channel2) = create_loopback_pair(0x1234_5678);

    kcp1.set_dead_link(3);

    kcp1.send(b"test data").unwrap();

    for i in 0..5 {
        simulate_tick(&mut kcp1, i * 200);
        channel2.drain();
    }

    assert!(
        kcp1.is_dead(),
        "Connection should be marked as dead after exceeding dead_link"
    );
}

#[test]
fn test_stream_mode_merge() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);
    kcp.set_stream(true);

    let handle1 = kcp.send_with_handle(b"hello").unwrap();
    let handle2 = kcp.send_with_handle(b" world").unwrap();

    assert!(handle2.sn_start <= handle1.sn_start + 1);

    let mut recv_buf = vec![0u8; 1024];
    let _ = kcp.recv(&mut recv_buf);
}

#[test]
fn test_window_probe() {
    let (mut kcp1, mut kcp2, channel1, channel2) = create_loopback_pair(0x1234_5678);

    kcp2.set_wndsize(32, 0);

    kcp1.send(b"test data").unwrap();
    simulate_tick(&mut kcp1, 0);

    let packets = channel2.drain();
    for packet in &packets {
        let _ = kcp2.input(packet);
    }

    simulate_tick(&mut kcp2, 10);

    let responses = channel1.drain();
    for response in &responses {
        let _ = kcp1.input(response);
    }

    simulate_tick(&mut kcp1, 20);
    let _probes = channel2.drain();
    assert!(!kcp1.is_dead(), "kcp should survive window probe scenario");
}

#[test]
fn test_time_diff_wrapping() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.send(b"test").unwrap();

    kcp.update(1000);
    kcp.flush();

    kcp.update(2000);
    kcp.flush();

    assert!(!kcp.is_dead(), "kcp should not be dead after time diff wrapping");
    kcp.send(b"still alive").unwrap();
}

#[test]
fn test_segment_encode_decode_roundtrip() {
    let mut seg = Segment::new();
    seg.conv = 0x1122_3344;
    seg.cmd = 81;
    seg.frg = 0;
    seg.wnd = 128;
    seg.ts = 1000;
    seg.sn = 1;
    seg.una = 0;
    seg.data = vec![1, 2, 3, 4, 5];

    let mut buffer = [0u8; 256];
    let written = seg.encode_to_slice(&mut buffer).unwrap();

    let (decoded, consumed) = Segment::decode_from_slice(&buffer[..written]).unwrap();
    assert_eq!(consumed, written);

    assert_eq!(seg.conv, decoded.conv);
    assert_eq!(seg.cmd, decoded.cmd);
    assert_eq!(seg.frg, decoded.frg);
    assert_eq!(seg.wnd, decoded.wnd);
    assert_eq!(seg.ts, decoded.ts);
    assert_eq!(seg.sn, decoded.sn);
    assert_eq!(seg.una, decoded.una);
    assert_eq!(seg.data, decoded.data);
}

#[test]
fn test_segment_decode_from_slice_roundtrip() {
    let mut seg = Segment::new();
    seg.conv = 0x1122_3344;
    seg.cmd = 81;
    seg.frg = 0;
    seg.wnd = 128;
    seg.ts = 1000;
    seg.sn = 1;
    seg.una = 0;
    seg.data = vec![1, 2, 3, 4, 5];

    let mut buffer = [0u8; 256];
    let written = seg.encode_to_slice(&mut buffer).unwrap();

    let (decoded, consumed) = Segment::decode_from_slice(&buffer[..written]).unwrap();

    assert_eq!(consumed, written);
    assert_eq!(seg.conv, decoded.conv);
    assert_eq!(seg.cmd, decoded.cmd);
    assert_eq!(seg.frg, decoded.frg);
    assert_eq!(seg.wnd, decoded.wnd);
    assert_eq!(seg.ts, decoded.ts);
    assert_eq!(seg.sn, decoded.sn);
    assert_eq!(seg.una, decoded.una);
    assert_eq!(seg.data, decoded.data);
}

#[test]
fn test_segment_decode_from_slice_truncated() {
    let mut seg = Segment::new();
    seg.conv = 0x1122_3344;
    seg.cmd = 81;
    seg.frg = 0;
    seg.wnd = 128;
    seg.ts = 1000;
    seg.sn = 1;
    seg.una = 0;
    seg.data = vec![1, 2, 3, 4, 5];

    let mut buffer = [0u8; 256];
    let written = seg.encode_to_slice(&mut buffer).unwrap();

    let truncated = &buffer[..written - 2];
    let result = Segment::decode_from_slice(truncated);
    assert!(result.is_err());
}

#[test]
fn test_segment_decode_from_slice_header_too_short() {
    let short_data = vec![0u8; 10];
    let result = Segment::decode_from_slice(&short_data);
    assert!(result.is_err());
}

#[test]
fn test_kcp_set_mtu() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.set_mtu(1000).unwrap();

    let result = kcp.set_mtu(10);
    assert!(result.is_err());
}

#[test]
fn test_kcp_set_wndsize() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.set_wndsize(64, 64);
    kcp.send(b"test").unwrap();
    assert!(kcp.wait_snd() > 0);
    assert!(!kcp.is_dead());
}

#[test]
fn test_kcp_set_nodelay() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.set_nodelay(true, 20, 2, true);
    kcp.send(b"test").unwrap();
    assert!(kcp.wait_snd() > 0);
    assert!(!kcp.is_dead());
}

#[test]
fn test_kcp_set_interval() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.set_interval(100);
    kcp.send(b"test").unwrap();
    assert!(kcp.wait_snd() > 0);
    assert!(!kcp.is_dead());
}

#[test]
fn test_kcp_set_rx_minrto() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.set_rx_minrto(100);
    kcp.send(b"test").unwrap();
    assert!(kcp.wait_snd() > 0);
    assert!(!kcp.is_dead());
}

#[test]
fn test_kcp_set_dead_link() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.set_dead_link(5);
    kcp.send(b"test").unwrap();
    assert!(kcp.wait_snd() > 0);
    assert!(!kcp.is_dead());
}

#[test]
fn test_kcp_state_and_is_dead() {
    let output = |_: &[u8]| {};
    let kcp = Kcp::new(0x1234_5678, output);

    assert_eq!(kcp.state(), 0);
    assert!(!kcp.is_dead());
}

#[test]
fn test_kcp_wait_snd() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    assert_eq!(kcp.wait_snd(), 0);

    kcp.send(b"test").unwrap();
    assert!(kcp.wait_snd() > 0);
}

#[test]
fn test_kcp_reset_rto() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.reset_rto();
    kcp.send(b"test").unwrap();
    assert!(kcp.wait_snd() > 0);
    assert!(!kcp.is_dead());
}

#[test]
fn test_kcp_conv() {
    let output = |_: &[u8]| {};
    let kcp = Kcp::new(0x1234_5678, output);

    assert_eq!(kcp.conv(), 0x1234_5678);
}

#[test]
fn test_kcp_get_conv() {
    let mut seg = Segment::new();
    seg.conv = 0x1234_5678;
    seg.cmd = 81;
    seg.frg = 0;
    seg.wnd = 128;
    seg.ts = 1000;
    seg.sn = 1;
    seg.una = 0;
    seg.data = vec![1, 2, 3];

    let mut buffer = [0u8; 256];
    let written = seg.encode_to_slice(&mut buffer).unwrap();

    let conv = Kcp::<fn(&[u8])>::get_conv(&buffer[..written]);
    assert_eq!(conv, Some(0x1234_5678));
}

#[test]
fn test_kcp_peek_size() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.send(b"test").unwrap();
    kcp.update(0);
    kcp.flush();

    let result = kcp.peek_size();
    assert!(matches!(result, Err(KcpError::RecvQueueEmpty)));
}

#[test]
fn test_kcp_send_empty_data() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    let result = kcp.send(b"");
    assert!(matches!(result, Err(KcpError::EmptyData)));
}

#[test]
fn test_kcp_input_too_short() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    let short_data = vec![0u8; 10];
    let result = kcp.input(&short_data);
    assert!(matches!(result, Err(KcpError::InputTooShort { .. })));
}

#[test]
fn test_kcp_recv_queue_empty() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    let mut buf = vec![0u8; 1024];
    let result = kcp.recv(&mut buf);
    assert!(matches!(result, Err(KcpError::RecvQueueEmpty)));
}

#[test]
fn test_kcp_buffer_too_small() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    // Build a minimal KCP CMD_PUSH segment to inject into rcv_queue.
    // Segment format: conv(4) cmd(1) frg(1) wnd(2) ts(4) sn(4) una(4) len(4) data(N)
    let data = b"test message";
    let overhead: usize = 24;
    let cmd_push: u8 = 81;
    let mut seg = vec![0u8; overhead + data.len()];
    seg[0..4].copy_from_slice(&0x1234_5678u32.to_le_bytes()); // conv
    seg[4] = cmd_push;   // cmd = CMD_PUSH
    seg[5] = 0;          // frg = 0 (last)
    seg[6..8].copy_from_slice(&128u16.to_le_bytes()); // wnd
    seg[12..16].copy_from_slice(&0u32.to_le_bytes()); // sn = 0
    seg[20..24].copy_from_slice(&(data.len() as u32).to_le_bytes()); // len
    seg[24..].copy_from_slice(data);

    kcp.input(&seg).unwrap();
    kcp.update(0);

    let mut small_buf = vec![0u8; 2];
    let result = kcp.recv(&mut small_buf);
    assert!(matches!(result, Err(KcpError::BufferTooSmall { .. })));
}

#[test]
fn test_kcp_send_too_many_fragments() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.set_mtu(100).unwrap();

    let large_data = vec![0u8; 10000];
    let result = kcp.send(&large_data);
    assert!(matches!(result, Err(KcpError::TooManyFragments { .. })));
}

#[test]
fn test_kcp_send_max_size() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    let max_data = vec![0u8; 65535];
    kcp.send(&max_data).unwrap();
    assert!(kcp.wait_snd() > 0, "data should be queued for send");
}

#[test]
fn test_kcp_stream_mode_large_data() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);
    kcp.set_stream(true);

    let large_data = vec![0u8; 10000];
    kcp.send(&large_data).unwrap();
    assert!(kcp.wait_snd() > 0);
}

#[test]
fn test_kcp_non_stream_mode_large_data() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);
    kcp.set_stream(false);

    let large_data = vec![0u8; 10000];
    kcp.send(&large_data).unwrap();
    assert!(kcp.wait_snd() > 0);
}

#[test]
fn test_kcp_update_large_time() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.update(1_000_000);
    kcp.flush();
    assert!(!kcp.is_dead(), "kcp should handle large time updates");
}

#[test]
fn test_kcp_check() {
    let output = |_: &[u8]| {};
    let kcp = Kcp::new(0x1234_5678, output);

    let next_update = kcp.check(1000);
    assert!(next_update >= 1000);
}

#[test]
#[allow(clippy::cast_possible_truncation)]
fn test_kcp_multiple_connections() {
    let mut connections = Vec::new();

    for i in 0..10 {
        let output = |_: &[u8]| {};
        let mut kcp = Kcp::new(0x1000 + i, output);
        kcp.send(&[i as u8; 100]).unwrap();
        connections.push(kcp);
    }

    for kcp in &mut connections {
        kcp.update(0);
        kcp.flush();
    }

    for (i, kcp) in connections.iter().enumerate() {
        assert_eq!(kcp.conv(), 0x1000 + i as u32, "conv should match");
        assert!(kcp.wait_snd() > 0, "connection {} should have pending data", i);
        assert!(!kcp.is_dead(), "connection {} should not be dead", i);
    }
}

#[test]
#[allow(clippy::cast_possible_truncation)]
fn test_kcp_async_multiple_connections() {
    use tokio::runtime::Runtime;

    let rt = Runtime::new().unwrap();

    rt.block_on(async {
        let mut connections = Vec::new();

        for i in 0..5 {
            let output = |_: &[u8]| {};
            let kcp = AsyncKcp::new(0x1000 + i, output);
            kcp.send(&[i as u8; 100]).await.unwrap();
            connections.push(kcp);
        }

        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
    });
}

#[test]
fn test_kcp_send_bytes_basic() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1122_3344, output);

    let data = b"hello kcp zero-copy";
    kcp.send(data).unwrap();

    assert!(kcp.wait_snd() > 0);
}

#[test]
fn test_kcp_input_bytes_roundtrip() {
    let buf1: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
    let buf2: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));

    let buf1_clone = buf1.clone();
    let buf2_clone = buf2.clone();

    let mut kcp1 = Kcp::new(0x1122_3344, move |data: &[u8]| {
        buf2_clone.borrow_mut().push(data.to_vec());
    });

    let mut kcp2 = Kcp::new(0x1122_3344, move |data: &[u8]| {
        buf1_clone.borrow_mut().push(data.to_vec());
    });

    kcp1.send(b"hello from kcp1 zero-copy").unwrap();
    kcp1.update(0);
    kcp1.flush();

    for pkt in buf2.borrow_mut().drain(..) {
        kcp2.input(&pkt).unwrap();
    }

    let mut recv_buf = vec![0u8; 1024];
    let size = kcp2.recv(&mut recv_buf).unwrap();
    assert_eq!(&recv_buf[..size], b"hello from kcp1 zero-copy");
}

#[test]
fn test_kcp_send_bytes_stream_mode() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);
    kcp.set_stream(true);

    kcp.send(b"hello").unwrap();
    kcp.send(b" world").unwrap();

    assert!(kcp.wait_snd() > 0);
}

// ─── CMD_RECONNECT 重连测试 ─────────────────────────────────

/// 设置快速模式（nodelay + nc），用于测试
fn set_fast_mode(kcp: &mut Kcp<impl Fn(&[u8])>) {
    kcp.set_nodelay(true, 10, 2, true);
    kcp.set_wndsize(512, 512);
}

/// 反复路由所有待处理包直到两端稳定，确保数据送达对端
fn drain_loop(
    kcp1: &mut Kcp<impl Fn(&[u8])>,
    kcp2: &mut Kcp<impl Fn(&[u8])>,
    ch1: &LoopbackChannel,
    ch2: &LoopbackChannel,
    start_time: u32,
) {
    for round in 0..100 {
        let current = start_time + round * 5;

        for pkt in ch2.drain() {
            let _ = kcp2.input(&pkt);
        }
        for pkt in ch1.drain() {
            let _ = kcp1.input(&pkt);
        }
        kcp1.update(current);
        kcp1.flush();
        kcp2.update(current);
        kcp2.flush();

        // 没有待处理包 + 两端都无未发送数据 → 稳定了
        if ch1.packets.borrow().is_empty()
            && ch2.packets.borrow().is_empty()
            && kcp1.wait_snd() == 0
            && kcp2.wait_snd() == 0
            && round > 2
        {
            break;
        }
    }
}

/// 全新连接上发送 `CMD_RECONNECT` —— 应为无操作，仅记录 `rmt_wnd`
#[test]
fn test_reconnect_fresh() {
    let (mut kcp1, mut kcp2, ch1, ch2) = create_loopback_pair(0x1234_5678);
    set_fast_mode(&mut kcp1);
    set_fast_mode(&mut kcp2);

    // kcp2 发送 CMD_RECONNECT，模拟客户端重连
    kcp2.send_reconnect().unwrap();
    // 路由 CMD_RECONNECT 给 kcp1
    drain_loop(&mut kcp1, &mut kcp2, &ch1, &ch2, 0);

    // 重连后应能正常通信
    kcp1.send(b"hello after reconnect").unwrap();
    drain_loop(&mut kcp1, &mut kcp2, &ch1, &ch2, 10);

    let mut buf = vec![0u8; 1024];
    let n = kcp2.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"hello after reconnect");
}

/// `CMD_RECONNECT` 应清空发送端的 `snd_buf`/`snd_queue`
#[test]
fn test_reconnect_clears_send_state() {
    let (mut kcp1, mut kcp2, ch1, ch2) = create_loopback_pair(0x1234_5678);
    set_fast_mode(&mut kcp1);
    set_fast_mode(&mut kcp2);

    // kcp1 模拟服务端，已发送大量数据（snd_nxt 前进，snd_buf 有数据）
    for i in 0..100 {
        let msg = format!("data-{i:03}");
        kcp1.send(msg.as_bytes()).unwrap();
    }
    kcp1.update(0);
    kcp1.flush();

    // 确认 kcp1 有未确认数据
    assert!(kcp1.wait_snd() > 0);

    // kcp2 发送 CMD_RECONNECT，模拟客户端重连
    kcp2.send_reconnect().unwrap();

    // 路由 CMD_RECONNECT 到 kcp1
    drain_loop(&mut kcp1, &mut kcp2, &ch1, &ch2, 10);

    // kcp1 收到 CMD_RECONNECT 后应清空所有缓冲
    assert_eq!(kcp1.wait_snd(), 0, "snd_buf/snd_queue should be cleared after reconnect");
}

/// `CMD_RECONNECT` 应清空接收端的 `rcv_queue`/`rcv_buf`
#[test]
fn test_reconnect_clears_recv_state() {
    let (mut kcp1, mut kcp2, ch1, ch2) = create_loopback_pair(0x1234_5678);
    set_fast_mode(&mut kcp1);
    set_fast_mode(&mut kcp2);

    // kcp1 发送数据给 kcp2
    kcp1.send(b"data before reconnect").unwrap();
    drain_loop(&mut kcp1, &mut kcp2, &ch1, &ch2, 0);

    // kcp2 应该收到数据
    let mut buf = vec![0u8; 1024];
    let n = kcp2.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"data before reconnect");

    // kcp1 再发一批数据，让 kcp2 的 rcv_queue 有数据
    kcp1.send(b"pending data 1").unwrap();
    kcp1.send(b"pending data 2").unwrap();
    drain_loop(&mut kcp1, &mut kcp2, &ch1, &ch2, 10);

    // 数据已到达 kcp2，但尚未 recv（在 rcv_queue 中）
    assert!(kcp2.peek_size().is_ok(), "kcp2 should have data in rcv_queue");

    // kcp1 发 CMD_RECONNECT → kcp2 收到，清空接收缓冲
    kcp1.send_reconnect().unwrap();
    drain_loop(&mut kcp1, &mut kcp2, &ch1, &ch2, 20);

    // 重连后 kcp2 的接收队列应被清空
    let result = kcp2.recv(&mut buf);
    assert!(matches!(result, Err(KcpError::RecvQueueEmpty)),
        "rcv_queue should be cleared after reconnect");
}

/// 重连后应能继续正常收发数据
///
/// `CMD_RECONNECT` 只重置接收方状态，因此需要两端互发来实现双向同步，
/// 模拟真实场景：客户端重建连接后，服务端重置状态并通过响应使客户端也同步。
#[test]
fn test_reconnect_then_communicate() {
    let (mut kcp1, mut kcp2, ch1, ch2) = create_loopback_pair(0x1234_5678);
    set_fast_mode(&mut kcp1);
    set_fast_mode(&mut kcp2);

    // 先发一批数据，建立状态
    for i in 0..5 {
        kcp1.send(format!("pre-{i}").as_bytes()).unwrap();
    }
    drain_loop(&mut kcp1, &mut kcp2, &ch1, &ch2, 0);
    // kcp2 收掉所有旧数据
    let mut buf = vec![0u8; 1024];
    loop {
        match kcp2.recv(&mut buf) {
            Ok(n) if n > 0 => {}
            _ => break,
        }
    }

    // 双向同步：kcp2 发 CMD_RECONNECT → kcp1 重置
    kcp2.send_reconnect().unwrap();
    // kcp1 也发 CMD_RECONNECT → kcp2 重置，使序列号对齐
    kcp1.send_reconnect().unwrap();
    drain_loop(&mut kcp1, &mut kcp2, &ch1, &ch2, 10);

    // 双方状态已清空，收发新数据
    kcp1.send(b"new data after reconnect").unwrap();
    drain_loop(&mut kcp1, &mut kcp2, &ch1, &ch2, 20);

    let n = kcp2.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"new data after reconnect",
        "should be able to communicate after reconnect");
}

/// 两次 `CMD_RECONNECT` 连续发送（双向同步）
#[test]
fn test_reconnect_double() {
    let (mut kcp1, mut kcp2, ch1, ch2) = create_loopback_pair(0x1234_5678);
    set_fast_mode(&mut kcp1);
    set_fast_mode(&mut kcp2);

    // 第一次重连：双向同步
    kcp2.send_reconnect().unwrap();
    kcp1.send_reconnect().unwrap();
    drain_loop(&mut kcp1, &mut kcp2, &ch1, &ch2, 0);

    // 发点数据
    kcp1.send(b"after first reconnect").unwrap();
    drain_loop(&mut kcp1, &mut kcp2, &ch1, &ch2, 10);
    let mut buf = vec![0u8; 1024];
    let n = kcp2.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"after first reconnect");

    // 第二次重连：双向同步
    kcp2.send_reconnect().unwrap();
    kcp1.send_reconnect().unwrap();
    drain_loop(&mut kcp1, &mut kcp2, &ch1, &ch2, 20);

    // 再次通信
    kcp1.send(b"after second reconnect").unwrap();
    drain_loop(&mut kcp1, &mut kcp2, &ch1, &ch2, 30);

    let n = kcp2.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"after second reconnect");
}

/// 普通首包（非 `CMD_RECONNECT`）不应重置 `snd_nxt` —— `is_fresh` fix 的回归测试
///
/// 这是 `network_sim` mode 1 (nc=true) 中触发的 bug：
/// kcp1 已发送数据（`snd_nxt` > 0, `snd_buf` 非空），
/// 收到 kcp2 的普通包作为首包时，`is_fresh` 错误地重置了 `snd_nxt` = seg.una，
/// 导致 flush 中断言失败。
#[test]
fn test_first_input_does_not_reset_snd_nxt() {
    let (mut kcp1, mut kcp2, ch1, ch2) = create_loopback_pair(0x1122_3344);

    // kcp1 先发数据，使 snd_nxt > 0，snd_buf 非空
    kcp1.send(b"packet-1").unwrap();
    kcp1.send(b"packet-2").unwrap();
    kcp1.send(b"packet-3").unwrap();
    kcp1.update(0);
    kcp1.flush();
    // kcp1.snd_nxt 现在 > 0，snd_buf 有数据

    // 手动构造一个普通 CMD_PUSH 包给 kcp1 作为首包输入
    // 方法：让 kcp2 发送数据作为回包，路由给 kcp1
    // 先把 kcp1 的输出喂给 kcp2，让 kcp2 产生 ACK/echo
    for pkt in ch2.drain() {
        let _ = kcp2.input(&pkt);
    }
    // kcp2 收到数据后会产生 ACK
    kcp2.update(10);
    kcp2.flush();

    // 现在 kcp2 的输出中有 ACK 包，路由回 kcp1
    // 这是 kcp1 的 FIRST input → 会触发 is_fresh 路径
    // 之前 bug：is_fresh 重置 snd_nxt，导致后续 flush assertion 失败
    for pkt in ch1.drain() {
        let _ = kcp1.input(&pkt);
    }

    // fix 验证：flush 不应 panic（assertion 应通过）
    kcp1.update(20);
    kcp1.flush(); // 之前这里会 panic: snd_buf.last().sn >= snd_nxt

    // 并且 kcp1 状态应正常，可以继续收发
    kcp1.send(b"after first input").unwrap();
    kcp1.update(30);
    kcp1.flush();

    for pkt in ch2.drain() {
        let _ = kcp2.input(&pkt);
    }
    kcp2.update(40);
    kcp2.flush();

    let mut buf = vec![0u8; 1024];
    let n = kcp2.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"packet-1", "kcp2 should receive first packet");
}

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

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

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
    for packet in &packets {
        kcp2.input(packet).unwrap();
    }

    simulate_tick(&mut kcp2, 10);

    let acks = channel1.drain();
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

    let mut buffer = Vec::new();
    seg.encode(&mut buffer).unwrap();

    let mut cursor = std::io::Cursor::new(&buffer);
    let decoded = Segment::decode(&mut cursor).unwrap();

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

    let mut buffer = Vec::new();
    seg.encode(&mut buffer).unwrap();

    let (decoded, consumed) = Segment::decode_from_slice(&buffer).unwrap();

    assert_eq!(consumed, buffer.len());
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

    let mut buffer = Vec::new();
    seg.encode(&mut buffer).unwrap();

    let truncated = &buffer[..buffer.len() - 2];
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
}

#[test]
fn test_kcp_set_nodelay() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.set_nodelay(true, 20, 2, true);
    kcp.send(b"test").unwrap();
}

#[test]
fn test_kcp_set_interval() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.set_interval(100);
    kcp.send(b"test").unwrap();
}

#[test]
fn test_kcp_set_rx_minrto() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.set_rx_minrto(100);
    kcp.send(b"test").unwrap();
}

#[test]
fn test_kcp_set_dead_link() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.set_dead_link(5);
    kcp.send(b"test").unwrap();
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

    let mut buffer = Vec::new();
    seg.encode(&mut buffer).unwrap();

    let conv = Kcp::<fn(&[u8])>::get_conv(&buffer);
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

    kcp.send(b"test message").unwrap();
    kcp.update(0);
    kcp.flush();

    let mut small_buf = vec![0u8; 2];
    let result = kcp.recv(&mut small_buf);
    assert!(matches!(result, Err(KcpError::RecvQueueEmpty)));
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
}

#[test]
fn test_kcp_stream_mode_large_data() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);
    kcp.set_stream(true);

    let large_data = vec![0u8; 10000];
    kcp.send(&large_data).unwrap();
}

#[test]
fn test_kcp_non_stream_mode_large_data() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);
    kcp.set_stream(false);

    let large_data = vec![0u8; 10000];
    kcp.send(&large_data).unwrap();
}

#[test]
fn test_kcp_update_large_time() {
    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1234_5678, output);

    kcp.update(1_000_000);
    kcp.flush();
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

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
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

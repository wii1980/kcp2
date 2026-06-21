//! 简单的KCP性能测试

use kcp2::{Kcp, Segment};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

fn main() {
    println!("KCP性能测试");
    println!("==========");

    // 测试1: Segment编码/解码性能
    test_segment_codec();

    // 测试2: 发送小数据包性能
    test_send_small_packets();

    // 测试3: 发送大数据包性能
    test_send_large_packets();

    // 测试4: 回环通信性能
    test_loopback_performance();

    // 测试5: 并发接收性能测试
    test_concurrent_receive_performance();

    // 测试6: 多连接并发测试
    test_multi_connection_performance();

    println!("\n所有测试完成!");
}

#[allow(clippy::cast_precision_loss)]
fn test_segment_codec() {
    println!("\n测试1: Segment编码/解码性能");

    let mut seg = Segment::new();
    seg.conv = 0x1122_3344;
    seg.cmd = 81; // CMD_PUSH
    seg.frg = 0;
    seg.wnd = 128;
    seg.ts = 1000;
    seg.sn = 1;
    seg.una = 0;
    seg.data = vec![0u8; 100];

    let iterations = 100_000;
    let start = Instant::now();

    for _ in 0..iterations {
        let mut buffer = [0u8; 200];
        let written = seg.encode_to_slice(&mut buffer).unwrap();
        let _ = Segment::decode_from_slice(&buffer[..written]).unwrap();
    }

    let duration = start.elapsed();
    let per_op = duration.as_nanos() as f64 / f64::from(iterations);

    println!("  迭代次数: {iterations}");
    println!("  总时间: {duration:.2?}");
    println!("  每次操作: {per_op:.2} ns");
    println!("  每秒操作: {:.2}", 1_000_000_000.0 / per_op);
}

#[allow(clippy::cast_precision_loss)]
fn test_send_small_packets() {
    println!("\n测试2: 发送小数据包性能 (100字节)");

    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1122_3344, output);

    let data = vec![0u8; 100];
    let iterations = 100_000;
    let start = Instant::now();

    for _ in 0..iterations {
        kcp.send(&data).unwrap();
    }

    let duration = start.elapsed();
    let per_op = duration.as_nanos() as f64 / f64::from(iterations);
    let throughput =
        (data.len() as f64 * f64::from(iterations)) / duration.as_secs_f64() / 1024.0 / 1024.0;

    println!("  迭代次数: {iterations}");
    println!("  总时间: {duration:.2?}");
    println!("  每次发送: {per_op:.2} ns");
    println!("  吞吐量: {throughput:.2} MB/s");
}

#[allow(clippy::cast_precision_loss)]
fn test_send_large_packets() {
    println!("\n测试3: 发送大数据包性能 (10KB)");

    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1122_3344, output);
    kcp.set_mtu(1500).unwrap();

    let data = vec![0u8; 10 * 1024];
    let iterations = 10_000;
    let start = Instant::now();

    for _ in 0..iterations {
        kcp.send(&data).unwrap();
    }

    let duration = start.elapsed();
    let per_op = duration.as_nanos() as f64 / f64::from(iterations);
    let throughput =
        (data.len() as f64 * f64::from(iterations)) / duration.as_secs_f64() / 1024.0 / 1024.0;

    println!("  迭代次数: {iterations}");
    println!("  总时间: {duration:.2?}");
    println!("  每次发送: {per_op:.2} ns");
    println!("  吞吐量: {throughput:.2} MB/s");
}

#[allow(clippy::cast_precision_loss)]
fn test_loopback_performance() {
    println!("\n测试4: 回环通信性能");

    let iterations = 10_000;
    let mut total_bytes = 0;
    let start = Instant::now();

    for _ in 0..iterations {
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

        // 发送数据
        let data = b"test message";
        total_bytes += data.len();

        kcp1.send(data).unwrap();
        kcp1.update(0);
        kcp1.flush();

        // 接收数据
        for pkt in buf2.borrow_mut().drain(..) {
            kcp2.input(&pkt).unwrap();
        }

        // 读取数据
        let mut recv_buf = vec![0u8; 1024];
        let _ = kcp2.recv(&mut recv_buf).unwrap();
    }

    let duration = start.elapsed();
    let per_op = duration.as_nanos() as f64 / f64::from(iterations);
    let throughput = (total_bytes as f64) / duration.as_secs_f64() / 1024.0 / 1024.0;

    println!("  迭代次数: {iterations}");
    println!("  总时间: {duration:.2?}");
    println!("  每次回环: {per_op:.2} ns");
    println!("  吞吐量: {throughput:.2} MB/s");
}

/// 并发接收性能测试
/// 模拟多个数据包同时到达，测试接收缓冲区的并发处理能力
#[allow(clippy::cast_sign_loss, clippy::cast_precision_loss)]
fn test_concurrent_receive_performance() {
    println!("\n测试5: 并发接收性能测试");

    let output = |_: &[u8]| {};
    let mut kcp = Kcp::new(0x1122_3344, output);
    kcp.set_wndsize(256, 256); // 扩大窗口以容纳更多并发数据

    // 创建多个带不同序号的数据包，模拟并发到达
    let packet_count = 1000;
    let mut packets = Vec::new();

    for i in 0..packet_count {
        let mut seg = Segment::new();
        seg.conv = 0x1122_3344;
        seg.cmd = 81; // CMD_PUSH
        seg.frg = 0;
        seg.wnd = 256;
        seg.ts = i as u32 * 10;
        seg.sn = i as u32; // 连续序号
        seg.una = 0;
        seg.data = vec![0u8; 100]; // 100字节数据

        let mut buffer = [0u8; 256];
        let written = seg.encode_to_slice(&mut buffer).unwrap();
        packets.push(buffer[..written].to_vec());
    }

    let iterations = 100;
    let start = Instant::now();

    for _ in 0..iterations {
        // 重置KCP状态
        let output = |_: &[u8]| {};
        let mut kcp = Kcp::new(0x1122_3344, output);
        kcp.set_wndsize(256, 256);

        // 并发输入所有数据包（模拟几乎同时到达）
        for packet in &packets {
            kcp.input(packet).unwrap();
        }
    }

    let duration = start.elapsed();
    let per_iteration = duration.as_nanos() as f64 / f64::from(iterations);
    let per_packet = per_iteration / f64::from(packet_count);

    println!("  数据包数量: {packet_count}");
    println!("  迭代次数: {iterations}");
    println!("  总时间: {duration:.2?}");
    println!("  每次迭代: {per_iteration:.2} ns");
    println!("  每个数据包: {per_packet:.2} ns");
}

/// 多连接并发测试
/// 模拟多个KCP连接同时工作
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::items_after_statements
)]
fn test_multi_connection_performance() {
    println!("\n测试6: 多连接并发测试");

    const CONNECTION_COUNT: usize = 100;
    const PACKETS_PER_CONNECTION: usize = 10;
    const DATA_SIZE: usize = 100; // 字节

    type KcpConn = Kcp<Box<dyn Fn(&[u8])>>;

    let start = Instant::now();

    // 创建多个连接的模拟环境
    let mut connections: Vec<(KcpConn, Vec<Vec<u8>>)> = Vec::new();

    for i in 0..CONNECTION_COUNT {
        let conv_id = 0x1000 + i as u32;
        let mut packets = Vec::new();

        let output = Box::new(move |_data: &[u8]| {
            // 在实际场景中这里会发送到网络
            // 这里我们只是收集包用于后续处理
        }) as Box<dyn Fn(&[u8])>;

        let mut kcp = Kcp::new(conv_id, output);
        kcp.set_wndsize(32, 32);

        // 为每个连接发送一些数据
        for j in 0..PACKETS_PER_CONNECTION {
            let data = vec![(i * PACKETS_PER_CONNECTION + j) as u8; DATA_SIZE];
            kcp.send(&data).unwrap();
            packets.push(data);
        }

        connections.push((kcp, packets));
    }

    // 模拟更新所有连接
    let update_iterations = 10;
    for _ in 0..update_iterations {
        for (kcp, _) in &mut connections {
            kcp.update(0);
            kcp.flush();
        }
    }

    let duration = start.elapsed();
    let total_packets = CONNECTION_COUNT * PACKETS_PER_CONNECTION;
    let per_connection = duration.as_nanos() as f64 / CONNECTION_COUNT as f64;
    let per_packet = duration.as_nanos() as f64 / total_packets as f64;

    println!("  连接数: {CONNECTION_COUNT}");
    println!("  每个连接数据包: {PACKETS_PER_CONNECTION}");
    println!("  总数据包: {total_packets}");
    println!("  总时间: {duration:.2?}");
    println!("  每个连接: {per_connection:.2} ns");
    println!("  每个数据包: {per_packet:.2} ns");
}

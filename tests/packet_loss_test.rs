//! 丢包测试：不间断发送 100 个数据包，验证对端是否全部成功接收。
//!
//! 运行: `cargo test --test packet_loss_test -- --nocapture`

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::uninlined_format_args,
    clippy::used_underscore_binding
)]

use std::cell::RefCell;
use std::rc::Rc;

use kcp2::Kcp;

const PACKET_COUNT: usize = 100;
const PACKET_SIZE: usize = 100;

#[test]
fn test_packet_loss_100_burst() {
    let channel1: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
    let channel2: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));

    let ch1 = channel1.clone();
    let ch2 = channel2.clone();

    // 两个 KCP 端点，通过回调函数交换数据
    let mut server = Kcp::new(0x1234_5678, move |data: &[u8]| {
        ch1.borrow_mut().push(data.to_vec());
    });

    let mut client = Kcp::new(0x1234_5678, move |data: &[u8]| {
        ch2.borrow_mut().push(data.to_vec());
    });

    server.set_nodelay(true, 10, 2, true);
    server.set_wndsize(512, 512);
    client.set_nodelay(true, 10, 2, true);
    client.set_wndsize(512, 512);

    // 预发送所有 100 个数据包
    for i in 0..PACKET_COUNT {
        let mut packet = vec![0u8; PACKET_SIZE];
        packet[..4].copy_from_slice(&(i as u32).to_le_bytes());
        client.send(&packet).unwrap();
    }
    println!("客户端已发送 {PACKET_COUNT} 个包");

    let mut current: u32 = 0;
    let mut recv_buf = vec![0u8; 2048];
    let mut server_recv_count = 0;

    for round in 0..10_000 {
        current += 10;

        // 只调用 update（内部触发 flush）
        client.update(current);
        server.update(current);

        // client -> server
        for pkt in channel2.borrow_mut().drain(..) {
            server.input(&pkt).unwrap();
        }

        // server -> client
        for pkt in channel1.borrow_mut().drain(..) {
            client.input(&pkt).unwrap();
        }

        // 服务端接收
        while let Ok(n) = server.recv(&mut recv_buf) {
            if n > 0 {
                server_recv_count += 1;
            }
        }

        // 客户端接收
        while let Ok(_n) = client.recv(&mut recv_buf) {}

        if server_recv_count >= PACKET_COUNT && client.wait_snd() == 0 {
            println!("\n全部完成! round={round}");
            break;
        }
    }

    let lost = PACKET_COUNT - server_recv_count;

    println!();
    println!("========================================");
    println!("          丢包测试结果");
    println!("========================================");
    println!("  发送:    {PACKET_COUNT} 个包");
    println!("  接收:    {server_recv_count} 个包");
    println!("  丢包:    {lost} 个包");
    println!("  丢包率:  {:.2}%", (lost as f64) / (PACKET_COUNT as f64) * 100.0);
    println!("========================================");

    assert_eq!(
        server_recv_count, PACKET_COUNT,
        "丢包测试失败: 服务端只收到 {server_recv_count}/{PACKET_COUNT} 个包"
    );

    println!("✅ 测试通过! {PACKET_COUNT} 个包全部成功到达。");
}

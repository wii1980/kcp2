//! KCP回环测试示例
//!
//! 运行: cargo run --example echo

use kcp2::{Kcp, SendHandle};
use std::cell::RefCell;
use std::rc::Rc;

fn main() {
    // 模拟网络通道
    let channel1: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
    let channel2: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));

    let ch1 = channel1.clone();
    let ch2 = channel2.clone();

    // 创建两个KCP端点
    let mut kcp1 = Kcp::new(0x1234_5678, move |data: &[u8]| {
        ch2.borrow_mut().push(data.to_vec());
    });

    let mut kcp2 = Kcp::new(0x1234_5678, move |data: &[u8]| {
        ch1.borrow_mut().push(data.to_vec());
    });

    // 设置快速模式
    kcp1.set_nodelay(true, 10, 2, true);
    kcp2.set_nodelay(true, 10, 2, true);

    let mut current: u32 = 0;
    let mut recv_buf = vec![0u8; 2048];

    // kcp1发送数据，使用 send_with_handle 跟踪确认状态
    let messages = ["Hello KCP!", "这是第二条消息", "Rust实现的KCP协议"];
    let mut handles: Vec<(SendHandle, &str)> = Vec::new();
    
    for msg in &messages {
        let handle = kcp1.send_with_handle(msg.as_bytes()).unwrap();
        println!("[kcp1] 发送: {} (sn: {}-{})", msg, handle.sn_start, handle.sn_end);
        handles.push((handle, msg));
    }

    // 模拟通信循环
    for round in 0..100 {
        current += 10;

        // 更新两端
        kcp1.update(current);
        kcp2.update(current);

        // kcp1 -> kcp2
        for pkt in channel2.borrow_mut().drain(..) {
            kcp2.input(&pkt).unwrap();
        }

        // kcp2 -> kcp1
        for pkt in channel1.borrow_mut().drain(..) {
            kcp1.input(&pkt).unwrap();
        }

        // 检查消息确认状态
        for (handle, msg) in &handles {
            if kcp1.is_send_acked(*handle) {
                println!("[kcp1] 消息已确认: \"{msg}\" (round {round})");
            }
        }
        // 移除已确认的
        handles.retain(|(h, _)| !kcp1.is_send_acked(*h));

        // kcp2接收数据
        while let Ok(size) = kcp2.recv(&mut recv_buf) {
            let msg = String::from_utf8_lossy(&recv_buf[..size]);
            println!("[kcp2] 收到: {msg}");

            // 回显
            let echo = format!("Echo: {msg}");
            kcp2.send(echo.as_bytes()).unwrap();
        }

        // kcp1接收回显
        while let Ok(size) = kcp1.recv(&mut recv_buf) {
            let msg = String::from_utf8_lossy(&recv_buf[..size]);
            println!("[kcp1] 收到回显: {msg}");
        }

        // 所有消息都已确认则提前退出
        if handles.is_empty() {
            break;
        }
    }

    println!("\n通信完成!");
}

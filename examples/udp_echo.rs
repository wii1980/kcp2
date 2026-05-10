//! KCP over UDP 回显示例（高层接口）
//!
//! 启动服务器: cargo run --example `udp_echo` server
//! 启动客户端: cargo run --example `udp_echo` client

use kcp2::{KcpConfig, KcpConnector, KcpListener};
use tokio::time::Duration;

const SERVER_ADDR: &str = "0.0.0.0:12345";
const CONNECT_ADDR: &str = "127.0.0.1:12345";
const CONV: u32 = 0x1122_3344;

fn make_config() -> KcpConfig {
    KcpConfig::default()
        .nodelay(true, 10, 2, true)
        .wndsize(512, 512)
        .rx_minrto(200)
        .dead_link(8)
        .timeout(Duration::from_secs(30))
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(std::string::String::as_str) {
        Some("server") => run_server().await,
        Some("client") => run_client().await,
        _ => {
            println!("用法:");
            println!("  服务器: cargo run --example udp_echo server");
            println!("  客户端: cargo run --example udp_echo client");
        }
    }
}

async fn run_server() {
    println!("[服务器] 启动在 {SERVER_ADDR}");

    let config = make_config();
    let listener = KcpListener::bind_with_config(SERVER_ADDR, config)
        .await
        .unwrap();

    let (conn, addr) = listener.accept().await.unwrap();
    println!("[服务器] 客户端连接: {} (conv: {})", addr, conn.conv());

    let mut recv_buf = vec![0u8; 2048];

    loop {
        match conn.recv(&mut recv_buf).await {
            Ok(size) if size > 0 => {
                let msg = String::from_utf8_lossy(&recv_buf[..size]);
                println!("[服务器] 收到: {msg}");

                let echo = format!("Echo: {msg}");
                let is_quit = msg == "quit";

                if is_quit {
                    // 使用 send_and_wait_ack 确保退出响应送达
                    conn.send_and_wait_ack(echo.as_bytes())
                        .await
                        .unwrap();
                    println!("[服务器] 回复: {echo}");
                    println!("[服务器] 确认收到，退出");
                    return;
                }
                conn.send(echo.as_bytes()).await.unwrap();
                println!("[服务器] 回复: {echo}");
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("[服务器] 读取错误: {e}");
                return;
            }
        }

        if conn.is_dead().await {
            println!("[服务器] 连接已断开");
            return;
        }
    }
}

async fn run_client() {
    println!("[客户端] 连接到 {CONNECT_ADDR}");

    let config = make_config();
    let connector = KcpConnector::new(CONNECT_ADDR)
        .unwrap()
        .with_config(config)
        .conv(CONV);

    let session = connector.connect().await.unwrap();
    let conn = session.connection().clone();
    println!("[客户端] 已连接到服务器");

    let messages = ["Hello KCP!", "第二条消息", "Rust + Tokio + KCP", "quit"];
    let mut recv_buf = vec![0u8; 2048];

    for msg in &messages {
        conn.send(msg.as_bytes()).await.unwrap();
        println!("[客户端] 发送: {msg}");

        match conn.recv(&mut recv_buf).await {
            Ok(size) if size > 0 => {
                let response = String::from_utf8_lossy(&recv_buf[..size]);
                println!("[客户端] 收到: {response}");

                if response.contains("quit") {
                    println!("[客户端] 完成!");
                    return;
                }
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("[客户端] 读取错误: {e}");
                return;
            }
        }
    }

}

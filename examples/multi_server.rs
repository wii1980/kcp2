//! 多连接 KCP over UDP 示例（支持 AEAD / DTLS 加密）
//!
//! 明文模式（默认）:
//!   cargo run --example `multi_server` -- server
//!   cargo run --example `multi_server` -- client
//!   cargo run --example `multi_server` -- multi
//!
//! AEAD 加密模式（需启用 aead feature）:
//!   cargo run --example `multi_server` --features aead -- aead-server [aes|chacha]
//!   cargo run --example `multi_server` --features aead -- aead-client [aes|chacha]
//!   cargo run --example `multi_server` --features aead -- aead-multi [aes|chacha]
//!
//! DTLS 加密模式（需启用 dtls feature）:
//!   cargo run --example `multi_server` --features dtls -- dtls-server
//!   cargo run --example `multi_server` --features dtls -- dtls-client
//!   cargo run --example `multi_server` --features dtls -- dtls-multi

use kcp2::{KcpConfig, KcpConnector, KcpListener};
use std::sync::Arc;
use tokio::time::Duration;

// ── 明文模式常量 ──
const SERVER_ADDR: &str = "0.0.0.0:12345";
const CONNECT_ADDR: &str = "127.0.0.1:12345";

const CLIENT_COUNT: usize = 5;
const CONV_BASE: u32 = 0x1000;

#[cfg(feature = "aead")]
const AEAD_KEY: [u8; 32] = [
    0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54,
    0x32, 0x10, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0xFE, 0xDC, 0xBA, 0x98,
    0x76, 0x54, 0x32, 0x10,
];

#[cfg(feature = "dtls")]
const DTLS_PSK: &[u8] = b"kcp2-demo-shared-secret";
#[cfg(feature = "dtls")]
const DTLS_IDENTITY: &str = "kcp2-demo";

// ── 配置构建 ──

fn make_config() -> KcpConfig {
    KcpConfig::default()
        .nodelay(true, 10, 2, true)
        .wndsize(512, 512)
        .rx_minrto(200)
        .dead_link(8)
        .timeout(Duration::from_secs(30))
}

#[cfg(feature = "aead")]
fn make_aead_config(use_chacha: bool) -> KcpConfig {
    use kcp2::crypto::KcpCrypto;

    let crypto: Arc<dyn KcpCrypto> = if use_chacha {
        use kcp2::crypto::ChaCha20Poly1305Crypto;
        Arc::new(ChaCha20Poly1305Crypto::new(&AEAD_KEY))
    } else {
        use kcp2::crypto::Aes256GcmCrypto;
        Arc::new(Aes256GcmCrypto::new(&AEAD_KEY))
    };

    KcpConfig::default()
        .crypto(crypto)
        .nodelay(true, 10, 2, true)
        .wndsize(512, 512)
        .timeout(Duration::from_secs(30))
}

#[cfg(feature = "aead")]
fn parse_aead_algo(args: &[String]) -> bool {
    matches!(args.get(2).map(String::as_str), Some("chacha"))
}

#[cfg(feature = "aead")]
fn algo_name(use_chacha: bool) -> &'static str {
    if use_chacha {
        "ChaCha20-Poly1305"
    } else {
        "AES-256-GCM"
    }
}

// ── 连接处理（共享） ──

#[allow(clippy::future_not_send)]
async fn handle_connection(
    conn: Arc<kcp2::KcpConnection>,
    on_cleanup: impl FnOnce() + Send + 'static,
) {
    let mut recv_buf = vec![0u8; 2048];
    let timeout = Duration::from_secs(30);

    loop {
        match tokio::time::timeout(timeout, conn.recv(&mut recv_buf)).await {
            Ok(Ok(size)) if size > 0 => {
                let msg = String::from_utf8_lossy(&recv_buf[..size]);
                println!(
                    "[服务器] 从 {} (conv: {}) 收到: {}",
                    conn.addr(),
                    conn.conv(),
                    msg
                );

                let echo = format!("Echo from server (conv: {}): {}", conn.conv(), msg);
                if let Err(e) = conn.send(echo.as_bytes()).await {
                    eprintln!("[服务器] 发送回显错误 ({}): {}", conn.addr(), e);
                    break;
                }

                if msg.trim() == "quit" {
                    println!("[服务器] 客户端 {} 请求退出", conn.addr());
                    break;
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                eprintln!("[服务器] 读取错误 ({}): {}", conn.addr(), e);
                break;
            }
            Err(_) => {
                eprintln!(
                    "[服务器] 连接超时 ({}): conv={}",
                    conn.addr(),
                    conn.conv()
                );
                break;
            }
        }

        if conn.is_dead().await {
            break;
        }
    }

    on_cleanup();

    println!(
        "[服务器] 连接处理结束: {} (conv: {})",
        conn.addr(),
        conn.conv()
    );
}

/// 创建连接清理回调，防止误删重连后的新连接
fn make_cleanup_callback(
    listener: Arc<KcpListener>,
    conn: &Arc<kcp2::KcpConnection>,
) -> impl FnOnce() + Send + 'static {
    let conv_id = conn.conv();
    let expected_addr = conn.addr();
    move || {
        if let Some(existing) = listener.get_connection(conv_id) {
            if existing.addr() == expected_addr {
                listener.remove_connection(conv_id);
            }
        }
    }
}

// ── 明文模式 ──

async fn run_server() {
    println!("[服务器] 启动在 {SERVER_ADDR}");

    let config = make_config();
    let listener = Arc::new(
        KcpListener::bind_with_config(SERVER_ADDR, config)
            .await
            .unwrap(),
    );

    loop {
        match listener.accept().await {
            Ok((conn, addr)) => {
                println!("[服务器] 新客户端连接: {} (conv: {})", addr, conn.conv());
                let on_cleanup = make_cleanup_callback(listener.clone(), &conn);
                tokio::spawn(handle_connection(conn, on_cleanup));
            }
            Err(e) => {
                eprintln!("[服务器] 接受连接错误: {e}");
            }
        }
    }
}

async fn run_client() {
    println!("[客户端] 连接到 {CONNECT_ADDR}");

    let conv = CONV_BASE + 1;
    let config = make_config();
    let connector = KcpConnector::new(CONNECT_ADDR)
        .unwrap()
        .with_config(config)
        .conv(conv);

    let session = connector.connect().await.unwrap();
    let client_conn = session.connection().clone();
    println!("[客户端] 已连接到服务器");

    let messages = ["Hello from client", "第二条消息", "测试多连接", "quit"];
    let mut recv_buf = vec![0u8; 2048];

    for msg in &messages {
        if let Err(e) = client_conn.send(msg.as_bytes()).await {
            eprintln!("[客户端] 发送错误: {e}");
            break;
        }
        println!("[客户端] 发送: {msg}");

        tokio::time::sleep(Duration::from_millis(500)).await;

        match client_conn.recv(&mut recv_buf).await {
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
                break;
            }
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
async fn run_multi_clients() {
    println!("[测试] 启动 {CLIENT_COUNT} 个并发客户端");

    let mut handles = Vec::new();

    for i in 0..CLIENT_COUNT {
        let handle = tokio::spawn(async move {
            println!("[客户端{i}] 启动");

            let conv = CONV_BASE + 100 + i as u32;
            let config = make_config();
            let connector = KcpConnector::new(CONNECT_ADDR)
                .unwrap()
                .with_config(config)
                .conv(conv);

            let session = match connector.connect().await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[客户端{i}] 连接失败: {e}");
                    return;
                }
            };
            let client_conn = session.connection().clone();

            let messages = [
                format!("Hello from client {i}"),
                format!("Message 2 from client {i}"),
                format!("Message 3 from client {i}"),
                "quit".to_string(),
            ];

            let mut recv_buf = vec![0u8; 2048];

            for (j, msg) in messages.iter().enumerate() {
                if j > 0 {
                    tokio::time::sleep(Duration::from_millis(1000 + i as u64 * 200)).await;
                }

                if let Err(e) = client_conn.send(msg.as_bytes()).await {
                    eprintln!("[客户端{i}] 发送错误: {e}");
                    break;
                }
                println!("[客户端{i}] 发送: {msg}");

                match client_conn.recv(&mut recv_buf).await {
                    Ok(size) if size > 0 => {
                        let response = String::from_utf8_lossy(&recv_buf[..size]);
                        println!("[客户端{i}] 收到: {response}");

                        if response.contains("quit") {
                            println!("[客户端{i}] 完成!");
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("[客户端{i}] 读取错误: {e}");
                        break;
                    }
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.await;
    }

    println!("[测试] 所有客户端完成");
}

// ── AEAD 模式 ──

#[cfg(feature = "aead")]
async fn run_aead_server(use_chacha: bool) {
    let algo = algo_name(use_chacha);
    println!("[AEAD 服务器] 启动在 {SERVER_ADDR} ({algo})");

    let config = make_aead_config(use_chacha);
    let listener = Arc::new(
        KcpListener::bind_with_config(SERVER_ADDR, config)
            .await
            .unwrap(),
    );

    loop {
        match listener.accept().await {
            Ok((conn, addr)) => {
                println!(
                    "[AEAD 服务器] 新客户端连接: {} (conv: {})",
                    addr,
                    conn.conv()
                );
                let on_cleanup = make_cleanup_callback(listener.clone(), &conn);
                tokio::spawn(handle_connection(conn, on_cleanup));
            }
            Err(e) => {
                eprintln!("[AEAD 服务器] 接受连接错误: {e}");
            }
        }
    }
}

#[cfg(feature = "aead")]
async fn run_aead_client(use_chacha: bool) {
    let algo = algo_name(use_chacha);
    println!("[AEAD 客户端] 连接到 {CONNECT_ADDR} ({algo})");

    let conv = CONV_BASE + 1;
    let config = make_aead_config(use_chacha);
    let connector = KcpConnector::new(CONNECT_ADDR)
        .unwrap()
        .with_config(config)
        .conv(conv);

    let session = connector.connect().await.unwrap();
    let client_conn = session.connection().clone();
    println!("[AEAD 客户端] 已连接到服务器");

    let messages = ["Hello AEAD!", "KCP + encrypted", "安全通道测试", "quit"];
    let mut recv_buf = vec![0u8; 2048];

    for msg in &messages {
        if let Err(e) = client_conn.send(msg.as_bytes()).await {
            eprintln!("[AEAD 客户端] 发送错误: {e}");
            break;
        }
        println!("[AEAD 客户端] 发送: {msg}");

        tokio::time::sleep(Duration::from_millis(500)).await;

        match client_conn.recv(&mut recv_buf).await {
            Ok(size) if size > 0 => {
                let response = String::from_utf8_lossy(&recv_buf[..size]);
                println!("[AEAD 客户端] 收到: {response}");

                if response.contains("quit") {
                    println!("[AEAD 客户端] 完成!");
                    return;
                }
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("[AEAD 客户端] 读取错误: {e}");
                break;
            }
        }
    }
}

#[cfg(feature = "aead")]
#[allow(clippy::cast_possible_truncation)]
async fn run_aead_multi(use_chacha: bool) {
    let algo = algo_name(use_chacha);
    println!("[AEAD 测试] 启动 {CLIENT_COUNT} 个并发客户端 ({algo})");

    let mut handles = Vec::new();

    for i in 0..CLIENT_COUNT {
        let handle = tokio::spawn(async move {
            println!("[AEAD 客户端{i}] 启动");

            let conv = CONV_BASE + 200 + i as u32;
            let config = make_aead_config(use_chacha);
            let connector = KcpConnector::new(CONNECT_ADDR)
                .unwrap()
                .with_config(config)
                .conv(conv);

            let session = match connector.connect().await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[AEAD 客户端{i}] 连接失败: {e}");
                    return;
                }
            };
            let client_conn = session.connection().clone();

            let messages = [
                format!("Hello from AEAD client {i}"),
                format!("Encrypted msg 2 from client {i}"),
                format!("Encrypted msg 3 from client {i}"),
                "quit".to_string(),
            ];

            let mut recv_buf = vec![0u8; 2048];

            for (j, msg) in messages.iter().enumerate() {
                if j > 0 {
                    tokio::time::sleep(Duration::from_millis(1000 + i as u64 * 200)).await;
                }

                if let Err(e) = client_conn.send(msg.as_bytes()).await {
                    eprintln!("[AEAD 客户端{i}] 发送错误: {e}");
                    break;
                }
                println!("[AEAD 客户端{i}] 发送: {msg}");

                match client_conn.recv(&mut recv_buf).await {
                    Ok(size) if size > 0 => {
                        let response = String::from_utf8_lossy(&recv_buf[..size]);
                        println!("[AEAD 客户端{i}] 收到: {response}");

                        if response.contains("quit") {
                            println!("[AEAD 客户端{i}] 完成!");
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("[AEAD 客户端{i}] 读取错误: {e}");
                        break;
                    }
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.await;
    }

    println!("[AEAD 测试] 所有客户端完成");
}

// ── DTLS 模式 ──

#[cfg(feature = "dtls")]
async fn run_dtls_server() -> Result<(), Box<dyn std::error::Error>> {
    use kcp2::transport::{DtlsConfig, DtlsServerTransport, KcpTransport};

    let dtls_cfg = DtlsConfig::server_psk(DTLS_PSK, DTLS_IDENTITY)
        .handshake_timeout(Duration::from_secs(5));
    let transport = Arc::new(DtlsServerTransport::bind(SERVER_ADDR, dtls_cfg).await?);
    println!("[DTLS 服务器] DTLS+KCP 监听在 {}", transport.local_addr()?);

    let kcp_cfg = make_config();
    let listener = Arc::new(KcpListener::from_transport(transport, kcp_cfg)?);

    loop {
        let (conn, peer) = listener.accept().await?;
        println!(
            "[DTLS 服务器] 新连接: peer={}, conv={}",
            peer,
            conn.conv()
        );
        let on_cleanup = make_cleanup_callback(listener.clone(), &conn);
        tokio::spawn(handle_connection(conn, on_cleanup));
    }
}

#[cfg(feature = "dtls")]
async fn run_dtls_client() -> Result<(), Box<dyn std::error::Error>> {
    use kcp2::transport::{DtlsClientTransport, DtlsConfig, KcpTransport};

    let dtls_cfg = DtlsConfig::client_psk(DTLS_PSK, DTLS_IDENTITY)
        .handshake_timeout(Duration::from_secs(5));
    let transport = Arc::new(DtlsClientTransport::connect(CONNECT_ADDR, dtls_cfg).await?);
    println!(
        "[DTLS 客户端] DTLS 握手完成, local={}",
        transport.local_addr()?
    );

    let kcp_cfg = make_config();
    let session = KcpConnector::from_transport(transport, CONNECT_ADDR, kcp_cfg)?
        .conv(CONV_BASE + 1)
        .connect()
        .await?;
    let client_conn = session.connection();
    println!(
        "[DTLS 客户端] KCP 会话已建立 (conv={})",
        client_conn.conv()
    );

    let messages = ["Hello DTLS!", "KCP over DTLS", "加密通道测试", "quit"];
    let mut recv_buf = vec![0u8; 4096];

    for msg in &messages {
        client_conn.send(msg.as_bytes()).await?;
        println!("[DTLS 客户端] 发送: {msg}");

        match tokio::time::timeout(Duration::from_secs(3), client_conn.recv(&mut recv_buf)).await {
            Ok(Ok(size)) if size > 0 => {
                let response = String::from_utf8_lossy(&recv_buf[..size]);
                println!("[DTLS 客户端] 收到: {response}");

                if response.contains("quit") {
                    println!("[DTLS 客户端] 完成!");
                    return Ok(());
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                eprintln!("[DTLS 客户端] 读取错误: {e}");
                break;
            }
            Err(_) => {
                eprintln!("[DTLS 客户端] 接收超时");
                break;
            }
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    Ok(())
}

#[cfg(feature = "dtls")]
#[allow(clippy::cast_possible_truncation)]
async fn run_dtls_multi() -> Result<(), Box<dyn std::error::Error>> {
    println!("[DTLS 测试] 启动 {CLIENT_COUNT} 个并发客户端");

    let mut handles = Vec::new();

    for i in 0..CLIENT_COUNT {
        let handle = tokio::spawn(async move {
            use kcp2::transport::{DtlsClientTransport, DtlsConfig};

            println!("[DTLS 客户端{i}] 启动");

            let conv = CONV_BASE + 300 + i as u32;

            let dtls_cfg = DtlsConfig::client_psk(DTLS_PSK, DTLS_IDENTITY)
                .handshake_timeout(Duration::from_secs(5));
            let transport = match DtlsClientTransport::connect(CONNECT_ADDR, dtls_cfg).await {
                Ok(t) => Arc::new(t),
                Err(e) => {
                    eprintln!("[DTLS 客户端{i}] DTLS 握手失败: {e}");
                    return;
                }
            };

            let kcp_cfg = make_config();
            let session =
                match KcpConnector::from_transport(transport, CONNECT_ADDR, kcp_cfg)
                    .unwrap()
                    .conv(conv)
                    .connect()
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[DTLS 客户端{i}] KCP 连接失败: {e}");
                        return;
                    }
                };
            let client_conn = session.connection().clone();

            let messages = [
                format!("Hello from DTLS client {i}"),
                format!("DTLS msg 2 from client {i}"),
                format!("DTLS msg 3 from client {i}"),
                "quit".to_string(),
            ];

            let mut recv_buf = vec![0u8; 4096];

            for (j, msg) in messages.iter().enumerate() {
                if j > 0 {
                    tokio::time::sleep(Duration::from_millis(1000 + i as u64 * 200)).await;
                }

                if let Err(e) = client_conn.send(msg.as_bytes()).await {
                    eprintln!("[DTLS 客户端{i}] 发送错误: {e}");
                    break;
                }
                println!("[DTLS 客户端{i}] 发送: {msg}");

                match tokio::time::timeout(
                    Duration::from_secs(3),
                    client_conn.recv(&mut recv_buf),
                )
                .await
                {
                    Ok(Ok(size)) if size > 0 => {
                        let response = String::from_utf8_lossy(&recv_buf[..size]);
                        println!("[DTLS 客户端{i}] 收到: {response}");

                        if response.contains("quit") {
                            println!("[DTLS 客户端{i}] 完成!");
                            break;
                        }
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        eprintln!("[DTLS 客户端{i}] 读取错误: {e}");
                        break;
                    }
                    Err(_) => {
                        eprintln!("[DTLS 客户端{i}] 接收超时");
                        break;
                    }
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.await;
    }

    println!("[DTLS 测试] 所有客户端完成");
    Ok(())
}

// ── 主入口 ──

fn print_help() {
    println!("多连接 KCP 示例（支持 AEAD / DTLS 加密）");
    println!();
    println!("明文模式:");
    println!("  cargo run --example multi_server -- server");
    println!("  cargo run --example multi_server -- client");
    println!("  cargo run --example multi_server -- multi");

    #[cfg(feature = "aead")]
    {
        println!();
        println!("AEAD 加密模式:");
        println!("  cargo run --example multi_server --features aead -- aead-server [aes|chacha]");
        println!("  cargo run --example multi_server --features aead -- aead-client [aes|chacha]");
        println!("  cargo run --example multi_server --features aead -- aead-multi [aes|chacha]");
    }

    #[cfg(feature = "dtls")]
    {
        println!();
        println!("DTLS 加密模式:");
        println!("  cargo run --example multi_server --features dtls -- dtls-server");
        println!("  cargo run --example multi_server --features dtls -- dtls-client");
        println!("  cargo run --example multi_server --features dtls -- dtls-multi");
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(std::string::String::as_str) {
        // 明文模式
        Some("server") => run_server().await,
        Some("client") => run_client().await,
        Some("multi") => run_multi_clients().await,

        // AEAD 模式
        #[cfg(feature = "aead")]
        Some("aead-server") => run_aead_server(parse_aead_algo(&args)).await,
        #[cfg(feature = "aead")]
        Some("aead-client") => run_aead_client(parse_aead_algo(&args)).await,
        #[cfg(feature = "aead")]
        Some("aead-multi") => run_aead_multi(parse_aead_algo(&args)).await,

        // DTLS 模式
        #[cfg(feature = "dtls")]
        Some("dtls-server") => {
            if let Err(e) = run_dtls_server().await {
                eprintln!("[DTLS 服务器] 错误: {e}");
            }
        }
        #[cfg(feature = "dtls")]
        Some("dtls-client") => {
            if let Err(e) = run_dtls_client().await {
                eprintln!("[DTLS 客户端] 错误: {e}");
            }
        }
        #[cfg(feature = "dtls")]
        Some("dtls-multi") => {
            if let Err(e) = run_dtls_multi().await {
                eprintln!("[DTLS 测试] 错误: {e}");
            }
        }

        // 帮助
        _ => print_help(),
    }
}

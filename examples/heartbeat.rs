//! KCP 心跳、消息接收和断开重连示例（支持 AEAD / DTLS 加密）
//!
//! 明文模式（默认）:
//!   先启动服务器: cargo run --example multi_server -- server
//!   再启动客户端: cargo run --example heartbeat
//!
//! AEAD 加密模式（需启用 aead feature）:
//!   先启动服务器: cargo run --example multi_server --features aead -- aead-server [aes|chacha]
//!   再启动客户端: cargo run --example heartbeat --features aead -- aead [aes|chacha]
//!
//! DTLS 加密模式（需启用 dtls feature）:
//!   先启动服务器: cargo run --example multi_server --features dtls -- dtls-server
//!   再启动客户端: cargo run --example heartbeat --features dtls -- dtls

use kcp2::{KcpConfig, KcpConnector, KcpSession};
#[cfg(any(feature = "aead", feature = "dtls"))]
use std::sync::Arc;
use std::future::Future;
use std::pin::Pin;
use tokio::time::Duration;

type ConnectFn = Box<dyn Fn() -> Pin<Box<dyn Future<Output = Result<KcpSession, String>> + Send>> + Send>;

// ── 明文模式常量 ──
const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:12345";
const CONV: u32 = 0x1122_3344;

// ── AEAD 模式常量 ──
#[cfg(feature = "aead")]
const AEAD_KEY: [u8; 32] = [
    0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54,
    0x32, 0x10, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0xFE, 0xDC, 0xBA, 0x98,
    0x76, 0x54, 0x32, 0x10,
];

// ── DTLS 模式常量 ──
#[cfg(feature = "dtls")]
const DTLS_PSK: &[u8] = b"kcp2-demo-shared-secret";
#[cfg(feature = "dtls")]
const DTLS_IDENTITY: &str = "kcp2-demo";

// ── 心跳常量 ──
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const MAX_CONSECUTIVE_FAILURES: u32 = 3;
const ACK_TIMEOUT: Duration = Duration::from_secs(5);
const INITIAL_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

// ── 配置构建 ──

fn make_config() -> KcpConfig {
    KcpConfig::default()
        .nodelay(true, 10, 2, false)
        .wndsize(256, 256)
        .rx_minrto(100)
        .dead_link(10)
        .stream(false)
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

    make_config().crypto(crypto)
}

#[cfg(feature = "aead")]
fn algo_name(use_chacha: bool) -> &'static str {
    if use_chacha {
        "ChaCha20-Poly1305"
    } else {
        "AES-256-GCM"
    }
}

// ── 连接函数 ──

/// 明文连接
async fn connect(server_addr: &str) -> Result<KcpSession, String> {
    let config = make_config();
    KcpConnector::new(server_addr)
        .map_err(|e| format!("地址解析失败: {e}"))?
        .with_config(config)
        .conv(CONV)
        .connect()
        .await
        .map_err(|e| format!("连接失败: {e}"))
}

/// AEAD 加密连接
#[cfg(feature = "aead")]
async fn connect_aead(server_addr: &str, use_chacha: bool) -> Result<KcpSession, String> {
    let config = make_aead_config(use_chacha);
    KcpConnector::new(server_addr)
        .map_err(|e| format!("地址解析失败: {e}"))?
        .with_config(config)
        .conv(CONV)
        .connect()
        .await
        .map_err(|e| format!("连接失败: {e}"))
}

/// DTLS 加密连接
#[cfg(feature = "dtls")]
async fn connect_dtls(server_addr: &str) -> Result<KcpSession, String> {
    use kcp2::transport::{DtlsClientTransport, DtlsConfig};

    let dtls_cfg = DtlsConfig::client_psk(DTLS_PSK.to_vec(), DTLS_IDENTITY)
        .handshake_timeout(Duration::from_secs(5));
    let transport = Arc::new(
        DtlsClientTransport::connect(server_addr, dtls_cfg)
            .await
            .map_err(|e| format!("DTLS 握手失败: {e}"))?,
    );

    let config = make_config();
    KcpConnector::from_transport(transport, server_addr, config)
        .map_err(|e| format!("创建连接器失败: {e}"))?
        .conv(CONV)
        .connect()
        .await
        .map_err(|e| format!("连接失败: {e}"))
}

// ── 心跳主循环 ──

async fn heartbeat_loop(session: KcpSession, mode_label: &str) -> String {
    if let Err(e) = session.connection().kcp().send_reconnect().await {
        eprintln!("[{mode_label}] send_reconnect 失败: {e}");
    }

    println!("[{mode_label}] 连接已建立，开始心跳");

    // 接收任务：独立接收服务端消息，仅打印
    let recv_conn = session.connection().clone();
    let label = mode_label.to_string();
    let mut recv_handle = tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        loop {
            if recv_conn.is_dead().await {
                return "KCP连接断开".to_string();
            }
            match recv_conn.recv(&mut buf).await {
                Ok(size) if size > 0 => {
                    let msg = String::from_utf8_lossy(&buf[..size]);
                    println!("[{label}] 收到消息: {msg}");
                }
                Err(e) => return format!("KCP接收错误: {e}"),
                _ => {}
            }
        }
    });

    // 心跳发送循环
    let mut heartbeat_counter: u32 = 0;
    let mut consecutive_failures: u32 = 0;
    let disconnected = loop {
        heartbeat_counter += 1;
        let heartbeat_msg = format!("HEARTBEAT_{heartbeat_counter}");
        println!("[{mode_label}] 发送心跳 #{heartbeat_counter}");

        match session
            .connection()
            .send_and_wait_ack_with_timeout(heartbeat_msg.as_bytes(), ACK_TIMEOUT)
            .await
        {
            Ok(()) => {
                println!("[{mode_label}] 心跳 #{heartbeat_counter} 已确认送达");
                consecutive_failures = 0;
            }
            Err(e) => {
                eprintln!("[{mode_label}] 心跳 #{heartbeat_counter} 发送失败: {e}");
                consecutive_failures += 1;
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    break format!("连续 {MAX_CONSECUTIVE_FAILURES} 次心跳发送失败");
                }
            }
        }

        tokio::select! {
            () = tokio::time::sleep(HEARTBEAT_INTERVAL) => {}
            reason = &mut recv_handle => {
                break reason.unwrap_or_else(|e| format!("接收任务异常: {e}"));
            }
        }
    };

    drop(recv_handle);
    drop(session);
    println!("[{mode_label}] 连接断开: {disconnected}");
    disconnected
}

// ── 重连循环 ──

async fn run_with_reconnect(connect_fn: ConnectFn, mode_label: &str) {
    let mut backoff_secs: u64 = INITIAL_BACKOFF.as_secs();

    loop {
        println!("[{mode_label}] 建立连接...");
        match connect_fn().await {
            Ok(session) => {
                backoff_secs = INITIAL_BACKOFF.as_secs();
                let _ = heartbeat_loop(session, mode_label).await;
            }
            Err(e) => {
                let jitter = rand::random::<u64>() % backoff_secs.max(1);
                eprintln!("[{mode_label}] {e}，{}s 后重连...", backoff_secs + jitter);
                tokio::time::sleep(Duration::from_secs(backoff_secs + jitter)).await;
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF.as_secs());
                continue;
            }
        }

        let jitter = rand::random::<u64>() % backoff_secs.max(1);
        let delay = backoff_secs + jitter;
        eprintln!("[{mode_label}] {}s 后重连...", delay);
        tokio::time::sleep(Duration::from_secs(delay)).await;
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF.as_secs());
    }
}

// ── 帮助 ──

fn print_help() {
    println!("KCP 心跳客户端（支持 AEAD / DTLS 加密）");
    println!();
    println!("明文模式:");
    println!("  cargo run --example heartbeat");
    println!("  (需先启动: cargo run --example multi_server -- server)");
    println!();
    println!("  可通过环境变量指定服务器地址:");
    println!("  KCP_SERVER_ADDR=192.168.1.1:12345 cargo run --example heartbeat");

    #[cfg(feature = "aead")]
    {
        println!();
        println!("AEAD 加密模式:");
        println!("  cargo run --example heartbeat --features aead -- aead [aes|chacha]");
        println!("  (需先启动: cargo run --example multi_server --features aead -- aead-server [aes|chacha])");
    }

    #[cfg(feature = "dtls")]
    {
        println!();
        println!("DTLS 加密模式:");
        println!("  cargo run --example heartbeat --features dtls -- dtls");
        println!("  (需先启动: cargo run --example multi_server --features dtls -- dtls-server)");
    }
}

// ── 主入口 ──

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        // 明文模式（默认）
        None => {
            let server_addr = std::env::var("KCP_SERVER_ADDR")
                .unwrap_or_else(|_| DEFAULT_SERVER_ADDR.to_string());
            println!("[明文] 目标服务器: {server_addr}");

            let addr = server_addr.clone();
            run_with_reconnect(
                Box::new(move || {
                    let addr = addr.clone();
                    Box::pin(async move { connect(&addr).await })
                }),
                "明文",
            )
            .await;
        }

        // AEAD 模式
        #[cfg(feature = "aead")]
        Some("aead") => {
            let use_chacha = matches!(args.get(2).map(|s| s.as_str()), Some("chacha"));
            let algo = algo_name(use_chacha);
            let server_addr = std::env::var("KCP_SERVER_ADDR")
                .unwrap_or_else(|_| DEFAULT_SERVER_ADDR.to_string());
            println!("[AEAD] 目标服务器: {server_addr} ({algo})");

            run_with_reconnect(
                Box::new(move || {
                    let addr = server_addr.clone();
                    Box::pin(async move { connect_aead(&addr, use_chacha).await })
                }),
                "AEAD",
            )
            .await;
        }

        // DTLS 模式
        #[cfg(feature = "dtls")]
        Some("dtls") => {
            let server_addr = std::env::var("KCP_SERVER_ADDR")
                .unwrap_or_else(|_| DEFAULT_SERVER_ADDR.to_string());
            println!("[DTLS] 目标服务器: {server_addr}");

            run_with_reconnect(
                Box::new(move || {
                    let addr = server_addr.clone();
                    Box::pin(async move { connect_dtls(&addr).await })
                }),
                "DTLS",
            )
            .await;
        }

        // 帮助
        _ => print_help(),
    }
}

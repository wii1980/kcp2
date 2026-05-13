//! KCP AEAD Echo 示例（AES-256-GCM / ChaCha20-Poly1305 整包加密）
//!
//! 启动服务端：
//!   cargo run --example `aead_echo` --features aead -- server
//!
//! 启动客户端：
//!   cargo run --example `aead_echo` --features aead -- client
//!
//! 双方使用相同的预共享密钥，KCP 数据包通过 AEAD 整包加密，
//! 无需握手，overhead 仅 32 字节/包。与原生 KCP 不互通。

#[cfg(not(feature = "aead"))]
fn main() {
    eprintln!("此示例需要启用 `aead` feature：");
    eprintln!("  cargo run --example aead_echo --features aead -- server|client");
    std::process::exit(1);
}

#[cfg(feature = "aead")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    const SERVER_ADDR: &str = "0.0.0.0:12346";
    const CONNECT_ADDR: &str = "127.0.0.1:12346";
    const CONV: u32 = 0xAADD_EEFF;

    // Shared secret key — in production, distribute out-of-band
    let key: [u8; 32] = [
        0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54,
        0x32, 0x10, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0xFE, 0xDC, 0xBA, 0x98,
        0x76, 0x54, 0x32, 0x10,
    ];

    let mode = std::env::args().nth(1).unwrap_or_else(|| "help".into());

    match mode.as_str() {
        "server" => run_server(&key, SERVER_ADDR).await,
        "client" => run_client(&key, CONNECT_ADDR, CONV).await,
        "server-chacha" => run_server_chacha(&key, SERVER_ADDR).await,
        "client-chacha" => run_client_chacha(&key, CONNECT_ADDR, CONV).await,
        _ => {
            println!("KCP AEAD Echo 示例");
            println!();
            println!("用法:");
            println!("  cargo run --example aead_echo --features aead -- server");
            println!("  cargo run --example aead_echo --features aead -- client");
            println!();
            println!("ChaCha20-Poly1305 模式:");
            println!("  cargo run --example aead_echo --features aead -- server-chacha");
            println!("  cargo run --example aead_echo --features aead -- client-chacha");
        }
    }

    Ok(())
}

#[cfg(feature = "aead")]
async fn run_server(key: &[u8; 32], addr: &str) {
    use kcp2::crypto::{Aes256GcmCrypto, KcpCrypto};
    use kcp2::{KcpConfig, KcpListener};
    use std::sync::Arc;
    use tokio::time::Duration;

    let crypto: Arc<dyn KcpCrypto> = Arc::new(Aes256GcmCrypto::new(key));
    println!(
        "[server] AEAD overhead: {} bytes/包 (AES-256-GCM)",
        crypto.overhead()
    );

    let config = KcpConfig::default()
        .crypto(crypto)
        .nodelay(true, 10, 2, true)
        .wndsize(512, 512)
        .timeout(Duration::from_secs(30));

    println!("[server] listening on {addr} (conv 明文保留用于路由)");
    let listener = KcpListener::bind_with_config(addr, config)
        .await
        .unwrap();

    loop {
        let (conn, peer) = listener.accept().await.unwrap();
        println!("[server] client connected: {peer} (conv={})", conn.conv());

        tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            loop {
                match conn.recv(&mut buf).await {
                    Ok(n) if n > 0 => {
                        let msg = String::from_utf8_lossy(&buf[..n]);
                        println!("[server] {peer} → {msg}");

                        let echo = format!("echo({peer}): {msg}");
                        let is_quit = msg == "quit";

                        if is_quit {
                            if conn.send_and_wait_ack(echo.as_bytes()).await.is_err() {
                                break;
                            }
                            println!("[server] replied: {echo}");
                            println!("[server] quit acked, closing");
                            return;
                        }
                        if conn.send(echo.as_bytes()).await.is_err() {
                            break;
                        }
                        println!("[server] replied: {echo}");
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("[server] recv error from {peer}: {e}");
                        return;
                    }
                }

                if conn.is_dead().await {
                    println!("[server] connection dead");
                    return;
                }
            }
        });
    }
}

#[cfg(feature = "aead")]
#[allow(clippy::similar_names)]
async fn run_client(key: &[u8; 32], addr: &str, conv: u32) {
    use kcp2::crypto::{Aes256GcmCrypto, KcpCrypto};
    use kcp2::{KcpConfig, KcpConnector};
    use std::sync::Arc;
    use tokio::time::Duration;

    let crypto: Arc<dyn KcpCrypto> = Arc::new(Aes256GcmCrypto::new(key));

    let config = KcpConfig::default()
        .crypto(crypto)
        .nodelay(true, 10, 2, true)
        .wndsize(512, 512)
        .timeout(Duration::from_secs(30));

    println!("[client] connecting to {addr} (conv={conv:#X}, AES-256-GCM)");
    let session = KcpConnector::new(addr)
        .unwrap()
        .with_config(config)
        .conv(conv)
        .connect()
        .await
        .unwrap();

    let conn = session.connection();
    println!("[client] KCP+AEAD session established");

    let messages = ["Hello AEAD!", "KCP + AES-256-GCM", "encrypted channel", "quit"];
    let mut buf = vec![0u8; 2048];

    for msg in &messages {
        conn.send(msg.as_bytes()).await.unwrap();
        println!("[client] → {msg}");

        match tokio::time::timeout(Duration::from_secs(3), conn.recv(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => {
                let resp = String::from_utf8_lossy(&buf[..n]);
                println!("[client] ← {resp}");
                if resp.contains("quit") {
                    println!("[client] done");
                    return;
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                eprintln!("[client] recv error: {e}");
                return;
            }
            Err(_) => {
                eprintln!("[client] recv timeout");
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(feature = "aead")]
async fn run_server_chacha(key: &[u8; 32], addr: &str) {
    use kcp2::crypto::{ChaCha20Poly1305Crypto, KcpCrypto};
    use kcp2::{KcpConfig, KcpListener};
    use std::sync::Arc;
    use tokio::time::Duration;

    let crypto: Arc<dyn KcpCrypto> = Arc::new(ChaCha20Poly1305Crypto::new(key));
    println!(
        "[server] AEAD overhead: {} bytes/包 (ChaCha20-Poly1305)",
        crypto.overhead()
    );

    let config = KcpConfig::default()
        .crypto(crypto)
        .nodelay(true, 10, 2, true)
        .wndsize(512, 512)
        .timeout(Duration::from_secs(30));

    println!("[server] listening on {addr} (ChaCha20-Poly1305)");
    let listener = KcpListener::bind_with_config(addr, config)
        .await
        .unwrap();

    loop {
        let (conn, peer) = listener.accept().await.unwrap();
        println!("[server] client connected: {peer} (conv={})", conn.conv());

        tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            loop {
                match conn.recv(&mut buf).await {
                    Ok(n) if n > 0 => {
                        let msg = String::from_utf8_lossy(&buf[..n]);
                        println!("[server] {peer} → {msg}");

                        let echo = format!("echo({peer}): {msg}");
                        if conn.send(echo.as_bytes()).await.is_err() {
                            break;
                        }
                        println!("[server] replied: {echo}");
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("[server] recv error from {peer}: {e}");
                        return;
                    }
                }

                if conn.is_dead().await {
                    println!("[server] connection dead");
                    return;
                }
            }
        });
    }
}

#[cfg(feature = "aead")]
#[allow(clippy::similar_names)]
async fn run_client_chacha(key: &[u8; 32], addr: &str, conv: u32) {
    use kcp2::crypto::{ChaCha20Poly1305Crypto, KcpCrypto};
    use kcp2::{KcpConfig, KcpConnector};
    use std::sync::Arc;
    use tokio::time::Duration;

    let crypto: Arc<dyn KcpCrypto> = Arc::new(ChaCha20Poly1305Crypto::new(key));

    let config = KcpConfig::default()
        .crypto(crypto)
        .nodelay(true, 10, 2, true)
        .wndsize(512, 512)
        .timeout(Duration::from_secs(30));

    println!("[client] connecting to {addr} (conv={conv:#X}, ChaCha20-Poly1305)");
    let session = KcpConnector::new(addr)
        .unwrap()
        .with_config(config)
        .conv(conv)
        .connect()
        .await
        .unwrap();

    let conn = session.connection();
    println!("[client] KCP+AEAD session established (ChaCha20-Poly1305)");

    let messages = ["Hello ChaCha!", "KCP + ChaCha20-Poly1305", "no hardware AES needed", "quit"];
    let mut buf = vec![0u8; 2048];

    for msg in &messages {
        conn.send(msg.as_bytes()).await.unwrap();
        println!("[client] → {msg}");

        match tokio::time::timeout(Duration::from_secs(3), conn.recv(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => {
                let resp = String::from_utf8_lossy(&buf[..n]);
                println!("[client] ← {resp}");
                if resp.contains("quit") {
                    println!("[client] done");
                    return;
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                eprintln!("[client] recv error: {e}");
                return;
            }
            Err(_) => {
                eprintln!("[client] recv timeout");
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

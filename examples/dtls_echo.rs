//! KCP over DTLS Echo 示例（PSK 模式）
//!
//! 启动服务端：
//!   cargo run --example `dtls_echo` --features dtls -- server
//!
//! 启动客户端：
//!   cargo run --example `dtls_echo` --features dtls -- client
//!
//! 双方使用相同的 PSK 完成 DTLS 1.2 握手，KCP 数据流在已加密通道之上传输。
//! 与原生 KCP 不互通；与 `kcp_echo` / `high_level_api` 示例对照可对比加密前后差异。

#[cfg(not(feature = "dtls"))]
fn main() {
    eprintln!("此示例需要启用 `dtls` feature：");
    eprintln!("  cargo run --example dtls_echo --features dtls -- server|client");
    std::process::exit(1);
}

#[cfg(feature = "dtls")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use kcp2::transport::{DtlsClientTransport, DtlsConfig, DtlsServerTransport, KcpTransport};
    use kcp2::{KcpConfig, KcpConnector, KcpListener};
    use std::sync::Arc;
    use tokio::time::Duration;

    const SERVER_ADDR: &str = "127.0.0.1:13443";
    const PSK: &[u8] = b"kcp2-demo-shared-secret";
    const IDENTITY: &str = "kcp2-demo";

    let kcp_cfg = KcpConfig::default()
        .nodelay(true, 10, 2, true)
        .wndsize(512, 512)
        .timeout(Duration::from_secs(30));

    let mode = std::env::args().nth(1).unwrap_or_else(|| "help".into());
    match mode.as_str() {
        "server" => {
            let dtls_cfg = DtlsConfig::server_psk(PSK.to_vec(), IDENTITY)
                .handshake_timeout(Duration::from_secs(5));
            let transport = Arc::new(DtlsServerTransport::bind(SERVER_ADDR, dtls_cfg).await?);
            println!("[server] DTLS+KCP listening on {}", transport.local_addr()?);
            let listener = KcpListener::from_transport(transport, kcp_cfg)?;

            loop {
                let (conn, peer) = listener.accept().await?;
                println!(
                    "[server] new DTLS+KCP connection: peer={}, conv={}",
                    peer,
                    conn.conv()
                );
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    loop {
                        match conn.recv(&mut buf).await {
                            Ok(n) if n > 0 => {
                                let msg = String::from_utf8_lossy(&buf[..n]);
                                println!("[server] {} → {}", peer, msg);
                                let echo = format!("echo({}): {}", peer, msg);
                                if conn.send(echo.as_bytes()).await.is_err() {
                                    break;
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                eprintln!("[server] recv err from {}: {}", peer, e);
                                break;
                            }
                        }
                    }
                });
            }
        }
        "client" => {
            let dtls_cfg = DtlsConfig::client_psk(PSK.to_vec(), IDENTITY)
                .handshake_timeout(Duration::from_secs(5));
            let transport = Arc::new(DtlsClientTransport::connect(SERVER_ADDR, dtls_cfg).await?);
            println!("[client] DTLS handshake done, local={}", transport.local_addr()?);

            let session = KcpConnector::from_transport(transport, SERVER_ADDR, kcp_cfg)?
                .conv(1)
                .connect()
                .await?;
            let conn = session.connection();
            println!("[client] KCP session established (conv=1)");

            for msg in ["hello", "world", "kcp", "over", "dtls"] {
                conn.send(msg.as_bytes()).await?;
                println!("[client] → {}", msg);

                let mut buf = vec![0u8; 4096];
                let n = tokio::time::timeout(Duration::from_secs(3), conn.recv(&mut buf))
                    .await
                    .map_err(|_| "recv timeout")??;
                println!("[client] ← {}", String::from_utf8_lossy(&buf[..n]));
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            println!("[client] done");
        }
        _ => {
            println!("Usage:");
            println!("  cargo run --example dtls_echo --features dtls -- server");
            println!("  cargo run --example dtls_echo --features dtls -- client");
        }
    }
    Ok(())
}

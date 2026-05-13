#![cfg(feature = "dtls")]
#![allow(clippy::module_name_repetitions)]

//! KCP over DTLS 集成测试
//!
//! 验证：
//! - DTLS 握手 → KCP Listener accept → 双向数据流
//! - 多客户端独立握手 + 路由
//! - PSK 不匹配时握手失败

use std::net::UdpSocket as StdUdpSocket;
use std::sync::Arc;
use std::time::Duration;

use kcp2_std::transport::{DtlsClientTransport, DtlsConfig, DtlsServerTransport, KcpTransport};
use kcp2_std::{KcpConfig, KcpConnector, KcpListener};

fn find_free_addr() -> String {
    let sock = StdUdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = sock.local_addr().unwrap();
    drop(sock);
    format!("127.0.0.1:{}", addr.port())
}

/// 端到端 KCP-over-DTLS：客户端发数据 → 服务端 listener.accept → 收到明文
#[tokio::test]
async fn test_kcp_over_dtls_echo() {
    let server_addr = find_free_addr();
    let psk = b"shared-test-secret".to_vec();

    let server_dtls = DtlsConfig::server_psk(&psk, "kcp2")
        .handshake_timeout(Duration::from_secs(5));
    let server_transport = Arc::new(
        DtlsServerTransport::bind(&server_addr, server_dtls)
            .await
            .expect("DTLS server bind"),
    );
    let listener_addr = server_transport.local_addr().unwrap();

    let kcp_cfg = KcpConfig::default()
        .nodelay(true, 10, 2, true)
        .wndsize(256, 256)
        .timeout(Duration::from_secs(5));
    let listener = KcpListener::from_transport(server_transport.clone(), kcp_cfg).unwrap();
    let listener = Arc::new(listener);

    // 服务端 echo task
    let listener_echo = listener.clone();
    let server_task = tokio::spawn(async move {
        let (conn, _peer) = listener_echo.accept().await.expect("accept");
        let mut buf = vec![0u8; 4096];
        let n = conn.recv(&mut buf).await.expect("server recv");
        conn.send(&buf[..n]).await.expect("server send");
    });

    // 客户端
    let client_dtls = DtlsConfig::client_psk(&psk, "kcp2")
        .handshake_timeout(Duration::from_secs(5));
    let client_transport = Arc::new(
        DtlsClientTransport::connect(&listener_addr.to_string(), client_dtls)
            .await
            .expect("DTLS client connect"),
    );

    let client_kcp_cfg = KcpConfig::default()
        .nodelay(true, 10, 2, true)
        .wndsize(256, 256)
        .timeout(Duration::from_secs(5));
    let session = KcpConnector::from_transport(
        client_transport,
        &listener_addr.to_string(),
        client_kcp_cfg,
    )
    .unwrap()
    .conv(7)
    .connect()
    .await
    .unwrap();

    let conn = session.connection();
    let payload = b"hello over dtls + kcp";
    conn.send(payload).await.expect("client send");

    let mut rbuf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(3), conn.recv(&mut rbuf))
        .await
        .expect("client recv timeout")
        .expect("client recv error");
    assert_eq!(&rbuf[..n], payload);

    // 等待 server task 收尾
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
}

/// 多客户端并发握手，每个独立 conv，互不干扰
#[tokio::test]
async fn test_kcp_over_dtls_multi_client() {
    let server_addr = find_free_addr();
    let psk = b"multi-client-secret".to_vec();

    let server_transport = Arc::new(
        DtlsServerTransport::bind(
            &server_addr,
            DtlsConfig::server_psk(&psk, "kcp2")
                .handshake_timeout(Duration::from_secs(5)),
        )
        .await
        .expect("DTLS server bind"),
    );
    let listener_addr = server_transport.local_addr().unwrap();

    let kcp_cfg = KcpConfig::default()
        .nodelay(true, 10, 2, true)
        .wndsize(256, 256)
        .timeout(Duration::from_secs(5));
    let listener = Arc::new(KcpListener::from_transport(server_transport.clone(), kcp_cfg).unwrap());

    // 服务端 accept N 个客户端
    let n_clients: u32 = 3;
    let listener_acc = listener.clone();
    let server_task = tokio::spawn(async move {
        for _ in 0..n_clients {
            let (conn, _peer) = listener_acc.accept().await.expect("accept");
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                if let Ok(n) = conn.recv(&mut buf).await {
                    let _ = conn.send(&buf[..n]).await;
                }
            });
        }
    });

    // 启动 N 个客户端
    let mut handles = Vec::new();
    for i in 1..=n_clients {
        let client_psk = psk.clone();
        let server_str = listener_addr.to_string();
        handles.push(tokio::spawn(async move {
            let client_transport = Arc::new(
                DtlsClientTransport::connect(
                    &server_str,
                    DtlsConfig::client_psk(&client_psk, "kcp2")
                        .handshake_timeout(Duration::from_secs(5)),
                )
                .await
                .expect("client connect"),
            );
            let session = KcpConnector::from_transport(
                client_transport,
                &server_str,
                KcpConfig::default()
                    .nodelay(true, 10, 2, true)
                    .wndsize(256, 256)
                    .timeout(Duration::from_secs(5)),
            )
            .unwrap()
            .conv(i)
            .connect()
            .await
            .unwrap();
            let conn = session.connection();

            let payload = format!("client-{i}").into_bytes();
            conn.send(&payload).await.unwrap();

            let mut rbuf = vec![0u8; 4096];
            let n = tokio::time::timeout(Duration::from_secs(3), conn.recv(&mut rbuf))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(&rbuf[..n], &payload[..]);
        }));
    }

    for h in handles {
        h.await.expect("client task panicked");
    }
    let _ = tokio::time::timeout(Duration::from_secs(3), server_task).await;
}

/// PSK 不匹配 → 握手失败
#[tokio::test]
async fn test_kcp_over_dtls_psk_mismatch() {
    let server_addr = find_free_addr();

    let _server = DtlsServerTransport::bind(
        &server_addr,
        DtlsConfig::server_psk(b"correct-secret", "kcp2")
            .handshake_timeout(Duration::from_secs(2)),
    )
    .await
    .expect("server bind");

    let result = DtlsClientTransport::connect(
        &server_addr,
        DtlsConfig::client_psk(b"wrong-secret", "kcp2")
            .handshake_timeout(Duration::from_secs(3)),
    )
    .await;

    assert!(result.is_err(), "handshake should fail when PSK differs");
}

#![cfg(feature = "aead")]
#![allow(clippy::module_name_repetitions)]

use std::net::UdpSocket as StdUdpSocket;
use std::sync::Arc;
use std::time::Duration;

use kcp2_std::crypto::{Aes256GcmCrypto, KcpCrypto};
use kcp2_std::{KcpConfig, KcpConnector, KcpListener};

fn find_free_addr() -> String {
    let sock = StdUdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = sock.local_addr().unwrap();
    drop(sock);
    format!("127.0.0.1:{}", addr.port())
}

/// 测试加密 KCP 全链路：服务端到客户端的数据收发
#[tokio::test]
async fn test_crypto_echo() {
    let server_key = Aes256GcmCrypto::generate_key().unwrap();

    let server_crypto = Arc::new(Aes256GcmCrypto::new(&server_key));
    let client_crypto = Arc::new(Aes256GcmCrypto::new(&server_key));

    // 服务端
    let server_addr = find_free_addr();
    let server_config = KcpConfig::default()
        .crypto(server_crypto)
        .nodelay(true, 10, 2, true)
        .wndsize(256, 256);
    let listener = KcpListener::bind_with_config(&server_addr, server_config)
        .await
        .unwrap();

    let server_conv = 42u32;

    // 客户端
    let client_config = KcpConfig::default()
        .crypto(client_crypto)
        .nodelay(true, 10, 2, true)
        .wndsize(256, 256)
        .timeout(Duration::from_secs(5));
    let session = KcpConnector::new(&server_addr)
        .unwrap()
        .with_config(client_config)
        .conv(server_conv)
        .connect()
        .await
        .unwrap();

    let conn = session.connection().clone();

    // 先创建服务端连接（模拟 accept 收到的第一个包触发了连接创建）
    let peer_addr = conn.addr();
    let _server_conn = listener.create_connection(server_conv, peer_addr);

    // 客户端发送数据
    let msg = b"hello encrypted kcp!";
    conn.send(msg).await.unwrap();

    // 服务端接收加密数据并验证解密正确
    let mut recv_buf = vec![0u8; 2048];
    let recv_result = tokio::time::timeout(
        Duration::from_secs(3),
        listener.recv_from(&mut recv_buf),
    )
    .await
    .expect("server recv_from timeout")
    .expect("server recv_from error");
    let (n, server_conn, _addr) = recv_result;
    assert_eq!(
        &recv_buf[..n],
        msg,
        "server should receive decrypted plaintext"
    );

    // 服务端 echo 回客户端
    server_conn.send(&recv_buf[..n]).await.unwrap();

    // 客户端接收 echo 并验证
    let mut client_buf = vec![0u8; 2048];
    let n = tokio::time::timeout(Duration::from_secs(3), conn.recv(&mut client_buf))
        .await
        .expect("client recv timeout")
        .expect("client recv error");
    assert_eq!(
        &client_buf[..n],
        msg,
        "client should receive echo back"
    );
}

/// 验证密钥不匹配时数据不可读
#[tokio::test]
async fn test_crypto_key_mismatch() {
    let key1 = Aes256GcmCrypto::generate_key().unwrap();
    let key2 = Aes256GcmCrypto::generate_key().unwrap();

    let crypto1 = Arc::new(Aes256GcmCrypto::new(&key1));
    let crypto2 = Arc::new(Aes256GcmCrypto::new(&key2));

    let addr = find_free_addr();
    let server_config = KcpConfig::default()
        .crypto(crypto1)
        .nodelay(true, 10, 2, true)
        .wndsize(256, 256);
    let listener = KcpListener::bind_with_config(&addr, server_config)
        .await
        .unwrap();

    let client_config = KcpConfig::default()
        .crypto(crypto2)
        .nodelay(true, 10, 2, true)
        .wndsize(256, 256)
        .timeout(Duration::from_secs(5));
    let session = KcpConnector::new(&addr)
        .unwrap()
        .with_config(client_config)
        .conv(1)
        .connect()
        .await
        .unwrap();

    let conn = session.connection();

    // 服务端创建连接接收客户端数据
    let peer_addr = conn.addr();
    listener.create_connection(1, peer_addr);

    let msg = b"should not be readable";
    conn.send(msg).await.unwrap();

    // 密钥不匹配：服务端无法解密 -> KCP input 静默丢弃，连接会因超时死亡
    let mut buf = vec![0u8; 2048];
    let recv_result = tokio::time::timeout(
        Duration::from_secs(2),
        listener.recv_from(&mut buf),
    )
    .await;

    assert!(
        recv_result.is_err(),
        "server should NOT receive readable data with mismatched keys (decrypt fails silently, kcp input dropped)"
    );
}

/// 验证加密后 wire 数据与明文不同
#[tokio::test]
async fn test_crypto_wire_different() {
    let key = Aes256GcmCrypto::generate_key().unwrap();

    let crypto = Arc::new(Aes256GcmCrypto::new(&key));
    let plaintext = b"visible on wire?";

    let encrypted = crypto.encrypt(42, plaintext).unwrap();
    assert_ne!(
        &encrypted[..],
        &plaintext[..],
        "encrypted data should differ from plaintext"
    );
    let conv_bytes: [u8; 4] = 42u32.to_le_bytes();
    assert_eq!(&encrypted[..4], &conv_bytes, "conv must be in plaintext");
    // 其余部分不应是明文
    assert_ne!(
        &encrypted[4..],
        &plaintext[..],
        "ciphertext must differ from plaintext"
    );
}

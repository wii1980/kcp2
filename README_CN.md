# rs-kcp2

[🇬🇧 English](README.md)

Rust 实现的 [KCP](https://github.com/skywind3000/kcp) 协议，支持 std / no_std / ESP32 全平台。

## 特性

- **三层分离架构**：`kcp2-core`（协议核心，no_std）→ `kcp2-std`（Tokio 异步）→ `kcp2-embassy`（Embassy 异步）
- **no_std 兼容**：核心协议零依赖 `std`，可在嵌入式设备直接运行
- **alloc / heapless 互斥**：`alloc` 模式使用 `Vec`（替代 `BTreeMap`）/`VecDeque`，`heapless` 模式使用固定大小容器，按需选择
- **Actor 模式**：每个连接独占一个 tokio task，通过 `mpsc` channel 通信，无锁操作 KCP 协议实例
- **Embassy 支持**：为 ESP32 等 no_std 平台提供完整的 Embassy 异步封装
- **ACK 追踪**：`send_and_wait_ack()` 精确等待消息确认
- **连接管理**：`KcpListener` 多路分发、自动过期清理、Builder 模式客户端
- **批量发送**：`send_batch()` 一次 channel 通信发送多条数据，减少往返开销
- **内存池**：Segment 内存池复用，减少分配开销
- **低每连接内存占用**：空闲连接仅 ~3.1 KB（优化前 ~17.4 KB），缓冲区延迟分配，channel/buffer 大小可调
- **发送背压**：`send_with_backpressure()` 在待发送分段数超过阈值时拒绝发送
- **无锁 Output**：使用 lock-free `ArrayQueue` 收集 KCP output
- **可插拔加密**：通过 `KcpCrypto` trait 支持 AES-256-GCM / ChaCha20-Poly1305 整包加密，feature-gated
- **可插拔传输层**：通过 `KcpTransport` trait 解耦 UDP socket，支持 DTLS / 自定义传输层

## 加密支持（`aead` feature）

`kcp2-std` 和 `kcp2-embassy` 均提供可选的整包 AEAD 加密层，对 KCP 协议透明。

### 启用

**std 平台：**

```toml
kcp2-std = { features = ["aead"] }
```

**嵌入式平台（ESP32）：**

```toml
kcp2-embassy = { features = ["aead"] }
```

### 使用示例

```rust
use kcp2_std::crypto::{Aes256GcmCrypto, KcpCrypto};
use std::sync::Arc;

// 生成 32 字节密钥（服务端和客户端需使用相同密钥）
let key = Aes256GcmCrypto::generate_key();
let crypto = Arc::new(Aes256GcmCrypto::new(&key));

// 服务端
let config = KcpConfig::default().crypto(crypto.clone());
let listener = KcpListener::bind_with_config("0.0.0.0:12345", config).await?;

// 客户端
let config = KcpConfig::default().crypto(crypto);
let session = KcpConnector::new("server:12345")?
    .with_config(config)
    .conv(1)
    .connect().await?;
```

### 数据包格式

加密后的 UDP 数据包格式为：

```
[CONV(4) | NONCE(12) | CIPHERTEXT | AEAD_TAG(16)]
```

- `CONV`(4): KCP 会话 ID，明文保留用于 Listener 路由
- `NONCE`(12): AEAD 随机数（计数器递增，保证唯一性）
- `CIPHERTEXT`: KCP segment(s) 加密数据
- `AEAD_TAG`(16): AEAD 认证标签

MTU 自动扣除 32 字节 overhead，无需手动调整。

### 已实现加密器

| 加密器 | 算法 | 平台适用性 |
|--------|------|---------|
| `Aes256GcmCrypto` | AES-256-GCM | x86_64 AES-NI 硬件加速；ESP32 有硬件 AES |
| `ChaCha20Poly1305Crypto` | ChaCha20-Poly1305 | 纯软件，无硬件依赖，嵌入式通用 |

### std 平台使用示例

```rust
use kcp2_std::crypto::{Aes256GcmCrypto, KcpCrypto};
use std::sync::Arc;

let key = Aes256GcmCrypto::generate_key();
let crypto = Arc::new(Aes256GcmCrypto::new(&key));

// 服务端
let config = KcpConfig::default().crypto(crypto.clone());
let listener = KcpListener::bind_with_config("0.0.0.0:12345", config).await?;

// 客户端
let config = KcpConfig::default().crypto(crypto);
let session = KcpConnector::new("server:12345")?
    .with_config(config)
    .conv(1)
    .connect().await?;
```

### Embassy 平台使用示例（ESP32）

```rust
use alloc::boxed::Box;
use kcp2_embassy::{EmbKcpSession, EmbKcpConfig};
use kcp2_embassy::crypto::{EmbKcpCrypto, ChaCha20Poly1305Crypto};

let key: [u8; 32] = [/* 32-byte pre-shared key */];
let crypto: Option<Box<dyn EmbKcpCrypto>> = Some(Box::new(ChaCha20Poly1305Crypto::new(&key)));

let config = EmbKcpConfig::embedded_constrained();
let mut session = EmbKcpSession::new_with_crypto(conv, socket, remote, config, crypto);

// 用法完全不变，加密透明
session.send(b"hello").unwrap();
let n = session.recv(&mut buf).await.unwrap();
```

> **注意**：`kcp2-embassy` 的 AEAD 实现使用 `Cell<u64>` nonce 计数器（非原子操作），兼容 riscv32imc 等无原子扩展的平台。数据包格式与 `kcp2-std` 完全一致，可跨平台互通。

## 传输层抽象

`KcpTransport` trait 将网络 I/O 从 `tokio::net::UdpSocket` 解耦：

```rust
pub trait KcpTransport: Send + Sync {
    fn try_send(&self, buf: &[u8]) -> io::Result<usize>;
    fn try_send_to(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize>;
    fn recv(&self, buf: &mut [u8]) -> impl Future<Output = io::Result<usize>>;
    fn recv_from(&self, buf: &mut [u8]) -> impl Future<Output = io::Result<(usize, SocketAddr)>>;
    fn local_addr(&self) -> io::Result<SocketAddr>;
    fn overhead(&self) -> usize { 0 }
}
```

`UdpTransport` 是默认实现，零开销包装 `Arc<UdpSocket>`。可通过 `from_transport()` 构造器传入自定义实现（如 DTLS）。

## 加密与传输层（可选）

`kcp2-std` 通过 **可插拔传输层 + 可选加密层** 提供端到端机密性。两种方案二选一：

| 方案 | feature | overhead | 握手 | 互操作 |
| --- | --- | --- | --- | --- |
| **整包 AEAD**（AES-256-GCM / ChaCha20-Poly1305） | `aead` | 32 字节 / 包 | 无（PSK 带外分发） | 自定义协议 |
| **DTLS 1.2**（PSK / 证书） | `dtls` | ~64 字节 / 包 | 有（标准握手 + 抗 DoS cookie） | IETF 标准 |
| 不加密（默认） | - | 0 | - | 与原生 KCP 互通 |

> ⚠️ 不要同时启用两者 — 会造成双重加密，浪费 CPU 和带宽。

### 整包 AEAD（feature `aead`）

```rust
use std::sync::Arc;
use kcp2::crypto::{Aes256GcmCrypto, ChaCha20Poly1305Crypto};
use kcp2::{KcpConfig, KcpListener};

let key = Aes256GcmCrypto::generate_key();
let crypto = Arc::new(Aes256GcmCrypto::new(&key));

let cfg = KcpConfig::default().crypto(crypto);  // MTU 自动扣除 32 字节
let listener = KcpListener::bind_with_config("0.0.0.0:12345", cfg).await?;
```

包格式：`[CONV(4) | NONCE(12) | CIPHERTEXT | AEAD_TAG(16)]`，conv 保留明文用于 Listener 路由。

### DTLS 1.2（feature `dtls`）

完整 DTLS 1.2 握手 + 加密通道，基于纯 Rust 的 [`webrtc-dtls`](https://crates.io/crates/webrtc-dtls)。

**服务端：**
```rust
use std::sync::Arc;
use kcp2::transport::{DtlsConfig, DtlsServerTransport};
use kcp2::{KcpConfig, KcpListener};

let dtls_cfg = DtlsConfig::server_psk(b"shared-secret".to_vec(), "kcp2");
let transport = Arc::new(DtlsServerTransport::bind("0.0.0.0:12345", dtls_cfg).await?);
let listener = KcpListener::from_transport(transport, KcpConfig::default())?;
let (conn, peer) = listener.accept().await?;
```

**客户端：**
```rust
use std::sync::Arc;
use kcp2::transport::{DtlsClientTransport, DtlsConfig};
use kcp2::{KcpConfig, KcpConnector};

let dtls_cfg = DtlsConfig::client_psk(b"shared-secret".to_vec(), "kcp2");
let transport = Arc::new(DtlsClientTransport::connect("server:12345", dtls_cfg).await?);
let session = KcpConnector::from_transport(transport, "server:12345", KcpConfig::default())?
    .conv(1)
    .connect()
    .await?;
```

完整 echo 示例：

```bash
cargo run --example dtls_echo --features dtls -- server
cargo run --example dtls_echo --features dtls -- client
```

要点：
- DTLS 默认套件 `TLS_PSK_WITH_AES_128_CCM_8`（IoT 友好，AEAD tag 仅 8 字节）。
- 服务端通过 `DTLSListener` 内置 HelloVerifyRequest cookie，抗 IP 反射放大。
- KCP MTU 自动扣除 `transport.overhead()`（默认 64 字节），避免 IP 分片。
- 与原生 KCP 不互通（DTLS 必须双方启用）。

### 自定义传输层

实现 [`KcpTransport`] trait 即可接入任意网络栈（Unix Domain、QUIC、Noise tunnel 等）：

```rust
pub trait KcpTransport: Send + Sync {
    fn try_send(&self, buf: &[u8]) -> io::Result<usize>;
    fn try_send_to(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize>;
    fn recv<'a>(&'a self, buf: &'a mut [u8]) -> Pin<Box<dyn Future<Output = io::Result<usize>> + Send + 'a>>;
    fn recv_from<'a>(&'a self, buf: &'a mut [u8]) -> Pin<Box<dyn Future<Output = io::Result<(usize, SocketAddr)>> + Send + 'a>>;
    fn local_addr(&self) -> io::Result<SocketAddr>;
    fn overhead(&self) -> usize { 0 }
}
```

## 协议扩展

本实现在标准 KCP 协议命令（PUSH=81, ACK=82, WASK=83, WINS=84）之外，增加了一个自定义扩展命令：

### `CMD_RECONNECT`（`0x80`）— 连接重连指令

当客户端断线后以相同 `conv` 重连时，服务端可能残留过期状态（发送缓冲、接收缓冲、序列号等），导致数据混乱。`CMD_RECONNECT` 提供了一种清洁的状态重置机制：

- **全新连接**：仅记录对端窗口，不做破坏性操作。
- **重连**（服务端已有有效状态）：完全清空所有发送/接收队列、ACK 列表，重置序列号为 0，恢复拥塞控制初始值，使连接回到初始状态。

`CMD_RECONNECT` 段为 **24 字节纯头部**（无数据 payload），网络开销极小。

#### 与标准 KCP 协议的兼容性

> **⚠️ `CMD_RECONNECT` 是 `kcp2` 项目的自定义扩展，不属于 [skywind3000/kcp](https://github.com/skywind3000/kcp) 原版协议。**

- 标准 KCP 仅有 4 个命令：`PUSH(81)`、`ACK(82)`、`WASK(83)`、`WINS(84)`。
- 此实现使用命令值 `0x80` (128) 以避免与标准命令（81-84）冲突。
- **与原生 KCP 实现（C/C++/Go 等）互操作时，对方不认识 `0x80`，会返回 `InvalidCmd` 错误。**
- 如需与标准 KCP 互操作，应**禁用此命令的发送**，或使对端也实现相应扩展。

#### 使用方法（kcp2-std）

```rust
// 客户端建立连接后，发送 CMD_RECONNECT 通知服务端初始化
session.connection().kcp().send_reconnect().await.unwrap();
```

#### 使用方法（kcp2-embassy）

```rust
// Embassy 环境下调用
session.send_reconnect().unwrap();
```

## 架构

```
┌──────────────────────────────────────────────────────────────┐
│                          用户代码                              │
├────────────────────┬─────────────────────┬───────────────────┤
│   kcp2-std          │   kcp2-embassy       │   kcp2-core        │
│   (Tokio + std)    │   (Embassy + no_std)│   (同步, 无运行时)  │
│                    │                     │                   │
│   KcpListener      │   EmbKcpSession     │   Kcp<Output>     │
│   KcpConnector     │   EmbKcpConfig      │                   │
│   KcpConnection    │   EmbassyClock      │                   │
│   KcpSession       │                     │                   │
├────────────────────┴─────────────────────┴───────────────────┤
│                     kcp2-core（协议核心）                        │
│              Kcp 协议实现 / Segment 编解码 / 常量               │
│         alloc_impl（动态容器） heapless_impl（固定容器）         │
├──────────────────────────────────────────────────────────────┤
│                        传输层（用户提供）                       │
│            tokio::net::UdpSocket | embassy-net UdpSocket      │
└──────────────────────────────────────────────────────────────┘
```

### 各层职责

| Crate | 目标平台 | 异步运行时 | 网络层 | 内存模型 |
|-------|---------|-----------|--------|---------|
| `kcp2-core` | no_std / std | 无（同步） | 无（output 回调） | alloc 或 heapless |
| `kcp2-std` | std (Linux/macOS/Windows) | Tokio | `tokio::net::UdpSocket` | alloc |
| `kcp2-embassy` | no_std (ESP32) | Embassy | `embassy-net::udp::UdpSocket` | alloc |

## 快速开始

### 添加依赖

**std 平台（默认）：**

```toml
[dependencies]
kcp2 = "0.2"
```

**std 平台 + 加密：**

```toml
[dependencies]
kcp2 = { version = "0.2", features = ["dtls"] }            # DTLS 1.2
# 或者
kcp2 = { version = "0.2", features = ["aead"] }            # 整包 AEAD
```

**嵌入式平台：**

```toml
[dependencies]
kcp2-core = { version = "0.1", default-features = false, features = ["alloc"] }
kcp2-embassy = "0.1"
```

**嵌入式平台 + AEAD 加密：**

```toml
[dependencies]
kcp2-core = { version = "0.1", default-features = false, features = ["alloc"] }
kcp2-embassy = { version = "0.1", features = ["aead"] }
```

**仅使用核心协议：**

```toml
[dependencies]
kcp2-core = { version = "0.1", default-features = false, features = ["heapless"] }
```

### 服务端（kcp2-std）

```rust
use kcp2::{KcpConfig, KcpListener};
use tokio::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = KcpConfig::default()
        .nodelay(true, 10, 2, true)
        .wndsize(512, 512)
        .timeout(Duration::from_secs(30));

    let listener = KcpListener::bind_with_config("0.0.0.0:12345", config).await?;

    loop {
        let (conn, addr) = listener.accept().await?;
        println!("新连接: {} (conv: {})", addr, conn.conv());

        tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            loop {
                match conn.recv(&mut buf).await {
                    Ok(size) if size > 0 => {
                        conn.send(&buf[..size]).await.unwrap();
                    }
                    Err(_) => break,
                    _ => {}
                }
            }
        });
    }
}
```

### 客户端（kcp2-std）

```rust
use kcp2::{KcpConfig, KcpConnector};
use tokio::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = KcpConfig::default()
        .nodelay(true, 10, 2, true)
        .timeout(Duration::from_secs(30));

    let session = KcpConnector::new("127.0.0.1:12345")?
        .with_config(config)
        .conv(1)
        .connect()
        .await?;

    let conn = session.connection();
    conn.send(b"Hello KCP").await?;

    let mut buf = vec![0u8; 2048];
    let n = conn.recv(&mut buf).await?;
    println!("收到: {}", String::from_utf8_lossy(&buf[..n]));

    session.close().await;
    Ok(())
}
```

### ACK 追踪

```rust
// 发送并等待对端确认
conn.send_and_wait_ack(b"important data").await?;

// 带超时的 ACK 等待
conn.send_and_wait_ack_with_timeout(b"important data", Duration::from_secs(5)).await?;
```

### Embassy 异步（ESP32）

```rust
use kcp2_embassy::{EmbKcpSession, EmbKcpConfig, EmbassyClock};

let config = EmbKcpConfig::embedded_constrained();

// 不加密
let session = EmbKcpSession::new(conv, socket, remote, config);

// 或启用 AEAD 加密
let crypto = Some(Box::new(ChaCha20Poly1305Crypto::new(&key)) as Box<dyn EmbKcpCrypto>);
let session = EmbKcpSession::new_with_crypto(conv, socket, remote, config, crypto);

session.send(b"hello").unwrap();
let n = session.recv(&mut buf).await.unwrap();

// 或手动驱动 update 循环
session.step().await;
```

### 底层 API（kcp2-core）

直接使用 `Kcp` 类型，无异步运行时和 socket 依赖：

```rust
use kcp2_core::Kcp;

let mut kcp = Kcp::new(conv, |data: &[u8]| {
    // 自定义 output 回调
});

kcp.set_nodelay(true, 10, 2, true);
kcp.set_wndsize(256, 256);

kcp.send(b"hello").unwrap();
kcp.update(current_millis);

// 收到 UDP 数据时调用
kcp.input(&recv_buf).unwrap();

let mut buf = vec![0u8; 2048];
let n = kcp.recv(&mut buf).unwrap();
```

## Feature Flags

### kcp2-core

| Feature | 依赖 | 说明 |
|---------|------|------|
| `std`（默认） | `alloc` | 完整 `std` 支持 |
| `alloc` | 无 | 仅 `alloc`，适合 ESP32 + `esp-alloc` |
| `heapless` | `heapless` | 无 `alloc`，固定大小容器（与 `alloc` 互斥） |
| `bytes` | `bytes` + `alloc` | `Bytes` 类型支持 |
| `fastack_conserve` | 无 | 快速 ACK 保护模式 |

### kcp2-std

| Feature | 依赖 | 说明 |
|---------|------|------|
| `fastack_conserve`（默认） | — | 快速 ACK 保护（透传至 kcp2-core） |
| `aead` | `aes-gcm`, `chacha20poly1305`, `getrandom` | 启用整包 AEAD（AES-256-GCM / ChaCha20-Poly1305），32 字节 overhead |
| `dtls` | `webrtc-dtls`, `webrtc-util` | 启用 DTLS 1.2 加密通道（PSK / 证书），~64 字节 overhead |

### kcp2-embassy

| Feature | 依赖 | 说明 |
|---------|------|------|
| `aead` | `aes-gcm`, `chacha20poly1305` | 启用整包 AEAD（AES-256-GCM / ChaCha20-Poly1305），32 字节 overhead |
| `esp32c3` | — | ESP32-C3 目标 |
| `esp32s3` | — | ESP32-S3 目标 |

**互斥关系**：`alloc` 和 `heapless` 互斥（kcp2-core）。`heapless_impl` 仅在 `!alloc && heapless` 时编译。

**各 crate 使用的 features：**
- `kcp2-std` 使用 `kcp-core { features = ["std", "bytes"] }`
- `kcp2-embassy` 使用 `kcp-core { default-features = false, features = ["alloc"] }`

## API 参考

### KcpListener（服务端，kcp2-std）

| 方法 | 说明 |
|------|------|
| `bind(addr)` | 绑定地址，使用默认配置 |
| `bind_with_config(addr, config)` | 绑定地址，自定义配置 |
| `from_socket(socket, config)` | 使用外部 UdpSocket |
| `from_transport(transport, config)` | 使用自定义传输层实现（如 DTLS） |
| `accept()` | 等待新连接，返回 `(KcpConnection, SocketAddr)` |
| `recv_from(buf)` | 收数据并返回 `(size, KcpConnection, addr)` |
| `create_connection(conv, addr)` | 手动创建连接 |
| `get_connection(conv)` | 按 conv 查找连接 |
| `remove_connection(conv)` | 移除连接 |
| `connection_count()` | 当前连接数 |
| `local_addr()` | 获取本地绑定地址 |
| `close()` | 关闭监听器 |

### KcpConnector（客户端，kcp2-std）

| 方法 | 说明 |
|------|------|
| `new(addr)` | 创建连接器 |
| `from_socket(socket, addr, config)` | 使用外部 UdpSocket |
| `from_transport(transport, addr, config)` | 使用自定义传输层实现 |
| `.conv(v)` / `.set_conv(v)` | 设置会话 ID |
| `.nodelay(...)` / `.set_nodelay(...)` | 配置 nodelay |
| `.wndsize(...)` / `.set_wndsize(...)` | 配置窗口 |
| `.timeout(d)` / `.set_timeout(d)` | 设置超时 |
| `.connect()` | 建立连接，返回 `KcpSession` |
| `.connect_with_handles()` | 额外返回 task JoinHandle |

### KcpSession（客户端会话，kcp2-std）

| 方法 | 说明 |
|------|------|
| `connection()` | 获取底层 `KcpConnection` |
| `close()` | 关闭会话，停止后台任务 |
| `is_alive()` | 连接是否存活 |
| `is_closed()` | 会话是否已关闭 |

### KcpConnection（连接抽象，kcp2-std）

| 方法 | 说明 |
|------|------|
| `send(data)` | 发送数据 |
| `send_with_backpressure(data)` | 带背压检查的发送（过载时返回 `Err(SendBackpressure)`） |
| `recv(buf)` | 接收数据（等待） |
| `try_recv(buf)` | 非阻塞接收 |
| `send_and_wait_ack(data)` | 发送并等待 ACK |
| `send_and_wait_ack_with_timeout(data, timeout)` | 带超时的 ACK 等待 |
| `wait_all_sent()` | 等待所有数据发送完毕 |
| `is_dead()` | 连接是否已死 |
| `close()` | 关闭连接 |

### EmbKcpConfig（kcp2-embassy）

**预设配置：**

| 预设方法 | 场景 |
|---------|------|
| `default()` | 通用场景 |
| `high_latency()` | 高延迟网络 |
| `high_loss()` | 高丢包网络 |
| `low_latency()` | 低延迟场景 |
| `embedded_constrained()` | 资源受限嵌入式设备 |

**Builder 方法：** `nodelay()`, `wndsize()`, `mtu()`, `timeout_ms()`

### AsyncKcp（底层 Actor 封装，kcp2-std）

`AsyncKcp` 是 kcp2-std 的内部 Actor 封装，将同步的 `Kcp` 实例包装为异步接口。一般无需直接使用，`KcpListener` 和 `KcpConnector` 已封装完整。

## 配置参数

### KcpConfig 字段

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `nodelay` | `bool` | `false` | 启用 nodelay 模式，降低 RTO |
| `interval` | `u32` | `100` | 内部时钟间隔（ms） |
| `resend` | `u32` | `0` | 快速重传阈值（0=禁用） |
| `nc` | `bool` | `false` | 禁用拥塞控制 |
| `sndwnd` | `u16` | `32` | 发送窗口大小 |
| `rcvwnd` | `u16` | `128` | 接收窗口大小 |
| `mtu` | `usize` | `1400` | 最大传输单元 |
| `rx_minrto` | `u32` | `100` | 最小 RTO（ms） |
| `dead_link` | `u32` | `10` | 最大重传次数 |
| `stream` | `bool` | `false` | 流模式 |
| `timeout` | `Duration` | `30s` | recv() 超时与空闲连接超时（`Duration::ZERO` = recv 无限等待） |
| `channel_capacity` | `usize` | `16` | 每连接 mpsc channel 容量（最小 4） |
| `max_wait_snd` | `usize` | `0` | 发送背压阈值，待发送分段数上限（0=禁用） |
| `pending_send_cap` | `usize` | `16` | 待发送缓冲区容量 |
| `crypto()` | builder | `None` | 注入 `KcpCrypto` 实现（feature `aead`），自动扣除 MTU overhead |

### 场景配置建议

**高延迟网络**（卫星链路、跨国连接）：

```rust
KcpConfig::default()
    .nodelay(true, 150, 2, false)
    .wndsize(512, 512)
    .rx_minrto(300)
    .dead_link(18)
```

**高丢包网络**（无线、移动网络）：

```rust
KcpConfig::default()
    .nodelay(true, 80, 1, true)
    .wndsize(256, 256)
    .rx_minrto(80)
    .dead_link(10)
```

**低延迟场景**（内网、同城）：

```rust
KcpConfig::default()
    .nodelay(true, 10, 2, true)
    .wndsize(512, 512)
    .rx_minrto(30)
    .dead_link(8)
```

## 使用注意事项

### Send 大小限制

`send()` 会将数据拆分为 KCP 分段。每次调用的最大分段数受接收窗口（`WND_RCV`，默认 128）限制。当 `stream` 模式启用且数据量超过 `WND_RCV × MSS`（默认约 176KB）时，`send()` 返回 `Err(TooManyFragments)` 而非静默丢弃部分数据。

```rust
// 错误：超过大小时会 panic
conn.send(&huge_data).await.unwrap();

// 正确：显式处理错误
match conn.send(&huge_data).await {
    Ok(()) => { /* 已发送 */ }
    Err(KcpError::TooManyFragments { .. }) => { /* 分片重试 */ }
    Err(e) => return Err(e.into()),
}
```

**建议**：传输大量数据时，在应用层将数据切分为远小于 `WND_RCV × MSS` 的块。

### 接收缓冲区必须足够大

`recv()` / `try_recv()` 在缓冲区不足时返回 `Err(BufferTooSmall { required, available })`。**数据不会被消费**——可用更大的缓冲区重试。

```rust
let mut buf = vec![0u8; 2048];
loop {
    match conn.recv(&mut buf).await {
        Ok(n) => { /* 处理 &buf[..n] */ }
        Err(KcpError::BufferTooSmall { required, .. }) => {
            buf.resize(required, 0);
            continue;  // 用更大的缓冲区重试
        }
        Err(e) => break,
    }
}
```

### recv() 超时

`recv()` 在 `KcpConfig::timeout()`（默认 30s）内无数据到达时返回 `Err(Timeout)`。连接不会关闭——再次调用 `recv()` 继续等待。设置 `KcpConfig::timeout(Duration::ZERO)` 可禁用 recv 超时。

```rust
let mut buf = vec![0u8; 2048];
loop {
    match conn.recv(&mut buf).await {
        Ok(n) => { /* 处理 &buf[..n] */ }
        Err(KcpError::Timeout) => { continue; }  // 暂无数据，继续等待
        Err(e) => break,
    }
}
```

### 不要忽略错误返回值

所有 I/O 方法（`send`、`recv`、`input`、`flush`）可能返回指示数据丢失或连接失败的错误。在非测试代码中用 `let _ =` 或 `.unwrap()` 忽略可能掩盖问题：

```rust
// 错误：静默丢弃发送失败
let _ = session.send(b"important data");

// 正确：处理或传播错误
session.send(b"important data")?;
// 或至少记录日志
if let Err(e) = session.send(b"important data") {
    log::error!("send failed: {e}");
}
```

这对 `kcp2-embassy` 尤其重要——`step()` 方法内部驱动 `input()` 和 `flush()`，确保 `log` 后端已初始化以捕获警告。

### Stream 模式边界行为

启用 `stream` 模式（`KcpConfig::stream(true)`）后，KCP 会将连续的 `send()` 调用合并到共享分段以提高效率。这意味着：

- 消息边界**不保留**——两次 100 字节 send 可能到达为一次 200 字节 recv，或以不同方式拆分。
- 如需消息帧边界，请在应用层实现（如长度前缀协议）。

## 错误类型

| 变体 | 说明 |
|------|------|
| `ConvMismatch` | 会话 ID 不匹配 |
| `InvalidCmd` | 无效命令 |
| `RecvQueueEmpty` | 接收队列为空 |
| `IncompletePacket` | 数据包不完整 |
| `DeadLink` | 连接已死 |
| `Timeout` | 操作超时 |
| `BufferTooSmall` | 缓冲区不足 |
| `TooManyFragments` | 数据过大，超出单次 send 上限（`WND_RCV × MSS`） |
| `SendBackpressure` | 待发送分段数过多（由 `send_with_backpressure()` 返回） |

## 示例

### std 平台

| 示例 | 说明 | 运行 |
|------|------|------|
| `echo` | 底层 Kcp 回环通信 | `cargo run --example echo` |
| `high_level_api` | KcpListener + KcpConnector Echo 服务 | `cargo run --example high_level_api server` |
| `heartbeat` | 心跳 + 断开重连 + 指数退避 | `cargo run --example heartbeat` |
| `multi_server` | 多连接服务端 | `cargo run --example multi_server server` |
| `udp_echo` | UDP Echo 基准 | `cargo run --example udp_echo` |
| `performance_test` | 性能测试 | `cargo run --example performance_test` |
| `dtls_echo` | KCP over DTLS 1.2 (PSK) | `cargo run --example dtls_echo --features dtls -- server` |

### ESP32 平台

| 示例 | 目标芯片 | 说明 | 编译 |
|------|---------|------|------|
| `embassy-esp32` | ESP32-C3 / S3 | Embassy 异步 KCP Echo 通信 | `./build.sh --chip c3` |
| `embassy-esp32-heartbeat` | ESP32-C3 / S3 | KCP 心跳 + 断线检测 + 自动重连 | `./build.sh --chip c3` |
| `embassy-esp32-heartbeat` | ESP32-C3 / S3 | 心跳 + AEAD 加密 (ChaCha20) | `./build.sh --chip c3 --aead` |
| `embassy-esp32-heartbeat` | ESP32-C3 / S3 | 心跳 + AEAD 加密 (AES-256-GCM) | `./build.sh --chip c3 --aes` |

## 依赖

### kcp2-core

| 库 | 用途 |
|----|------|
| `log` | 日志（no_std 兼容） |
| `heapless` | 可选，固定大小容器 |
| `bytes` | 可选，`Bytes` 类型支持 |

### kcp2-std

| 库 | 用途 |
|----|------|
| `kcp2-core` | 协议核心（std + bytes features） |
| `tokio` | 异步运行时 |
| `bytes` | 零拷贝缓冲 |
| `dashmap` | 并发 HashMap（连接表） |
| `crossbeam-queue` | 无锁队列（缓冲池） |
| `parking_lot` | 高性能 Mutex |
| `thiserror` | 错误类型派生 |
| `futures-util` | 异步工具 |
| `log` | 日志 |
| `aes-gcm` / `chacha20poly1305` | 整包 AEAD（仅 `aead` feature） |
| `webrtc-dtls` / `webrtc-util` | DTLS 1.2 加密通道（仅 `dtls` feature） |

### kcp2-embassy

| 库 | 用途 |
|----|------|
| `kcp2-core` | 协议核心（alloc feature） |
| `embassy-executor` | Embassy 异步执行器 |
| `embassy-time` | Embassy 定时器 |
| `embassy-net` | Embassy 网络栈 |
| `embassy-sync` | Embassy 同步原语 |
| `embassy-futures` | Embassy 异步工具 |
| `heapless` | 固定大小容器 |
| `static_cell` | 静态分配 |
| `log` | 日志 |
| `aes-gcm` / `chacha20poly1305` | 整包 AEAD（仅 `aead` feature） |

## 基准测试

```bash
cargo bench
```

包含两组测试：

- `kcp_benchmark`：协议层基准，测试 `send`/`recv`/`input`/`update` 性能
- `listener_benchmark`：监听器层基准，测试多连接场景吞吐

## QA 脚本

```bash
./qa.sh
```

执行 7 项检查：

1. `cargo check`：10 种 feature 组合逐一验证编译
2. `cargo clippy`：代码规范检查
3. `cargo test`：单元测试和集成测试
4. `cargo test --doc`：文档测试
5. `cargo run --example`：所有示例编译运行
6. `cargo bench`：基准测试编译验证
7. `cargo check -p kcp2-core --no-default-features`：no_std 兼容性检查

## License

MIT

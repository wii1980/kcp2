# rs-kcp2

[🇨🇳 中文版](README_CN.md)

A [KCP](https://github.com/skywind3000/kcp) protocol implementation in Rust, supporting std / no_std / ESP32 across all platforms.

## Features

- **Three-layer architecture**: `kcp2-core` (protocol core, no_std) → `kcp2-std` (Tokio async) → `kcp2-embassy` (Embassy async)
- **no_std compatible**: Core protocol has zero `std` dependencies, runs directly on embedded devices
- **alloc / heapless mutually exclusive**: `alloc` mode uses `Vec`/`VecDeque`, `heapless` mode uses fixed-size containers — choose as needed
- **Actor model**: Each connection runs in its own tokio task, communicates via `mpsc` channels, lock-free KCP instance access
- **Embassy support**: Full Embassy async wrapper for ESP32 and other no_std platforms
- **ACK tracking**: `send_and_wait_ack()` precisely waits for message acknowledgment
- **Connection management**: `KcpListener` for multiplexing, automatic expiry cleanup, Builder-pattern client
- **Batch send**: `send_batch()` sends multiple messages in a single channel transaction, reducing round-trip overhead
- **Memory pool**: Segment memory pool for reduced allocation overhead
- **Lock-free Output**: Uses lock-free `ArrayQueue` for KCP output collection
- **Pluggable encryption**: AES-256-GCM / ChaCha20-Poly1305 per-packet encryption via `KcpCrypto` trait, feature-gated
- **Pluggable transport**: `KcpTransport` trait decouples UDP sockets, supports DTLS / custom transports

## Encryption Support (`aead` feature)

Both `kcp2-std` and `kcp2-embassy` provide optional per-packet AEAD encryption, transparent to the KCP protocol.

### Enabling

**std platform:**

```toml
kcp2-std = { features = ["aead"] }
```

**Embedded platform (ESP32):**

```toml
kcp2-embassy = { features = ["aead"] }
```

### Usage Example

```rust
use kcp2_std::crypto::{Aes256GcmCrypto, KcpCrypto};
use std::sync::Arc;

// Generate a 32-byte key (server and client must share the same key)
let key = Aes256GcmCrypto::generate_key();
let crypto = Arc::new(Aes256GcmCrypto::new(&key));

// Server
let config = KcpConfig::default().crypto(crypto.clone());
let listener = KcpListener::bind_with_config("0.0.0.0:12345", config).await?;

// Client
let config = KcpConfig::default().crypto(crypto);
let session = KcpConnector::new("server:12345")?
    .with_config(config)
    .conv(1)
    .connect().await?;
```

### Packet Format

The encrypted UDP packet format is:

```
[CONV(4) | NONCE(12) | CIPHERTEXT | AEAD_TAG(16)]
```

- `CONV`(4): KCP session ID, kept in plaintext for Listener routing
- `NONCE`(12): AEAD nonce (incrementing counter, guaranteed unique)
- `CIPHERTEXT`: Encrypted KCP segment(s)
- `AEAD_TAG`(16): AEAD authentication tag

MTU is automatically reduced by 32 bytes for the overhead — no manual adjustment needed.

### Implemented Ciphers

| Cipher | Algorithm | Platform Suitability |
|--------|-----------|---------------------|
| `Aes256GcmCrypto` | AES-256-GCM | x86_64 AES-NI hardware acceleration; ESP32 has hardware AES |
| `ChaCha20Poly1305Crypto` | ChaCha20-Poly1305 | Pure software, no hardware dependency, universal for embedded |

### std Platform Example

```rust
use kcp2_std::crypto::{Aes256GcmCrypto, KcpCrypto};
use std::sync::Arc;

let key = Aes256GcmCrypto::generate_key();
let crypto = Arc::new(Aes256GcmCrypto::new(&key));

// Server
let config = KcpConfig::default().crypto(crypto.clone());
let listener = KcpListener::bind_with_config("0.0.0.0:12345", config).await?;

// Client
let config = KcpConfig::default().crypto(crypto);
let session = KcpConnector::new("server:12345")?
    .with_config(config)
    .conv(1)
    .connect().await?;
```

### Embassy Platform Example (ESP32)

```rust
use alloc::boxed::Box;
use kcp2_embassy::{EmbKcpSession, EmbKcpConfig};
use kcp2_embassy::crypto::{EmbKcpCrypto, ChaCha20Poly1305Crypto};

let key: [u8; 32] = [/* 32-byte pre-shared key */];
let crypto: Option<Box<dyn EmbKcpCrypto>> = Some(Box::new(ChaCha20Poly1305Crypto::new(&key)));

let config = EmbKcpConfig::embedded_constrained();
let mut session = EmbKcpSession::new_with_crypto(conv, socket, remote, config, crypto);

// Usage is identical — encryption is transparent
session.send(b"hello").unwrap();
let n = session.recv(&mut buf).await.unwrap();
```

> **Note**: `kcp2-embassy`'s AEAD implementation uses `Cell<u64>` for the nonce counter (non-atomic), compatible with platforms lacking atomic extensions like riscv32imc. The packet format is identical to `kcp2-std` — cross-platform interop works out of the box.

## Transport Abstraction

The `KcpTransport` trait decouples network I/O from `tokio::net::UdpSocket`:

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

`UdpTransport` is the default implementation, a zero-overhead wrapper around `Arc<UdpSocket>`. Pass custom implementations (e.g., DTLS) via the `from_transport()` constructor.

## Encryption & Transport Layer (Optional)

`kcp2-std` provides end-to-end confidentiality via **pluggable transport + optional encryption**. Choose one of the two schemes:

| Scheme | feature | overhead | Handshake | Interop |
|--------|---------|----------|-----------|---------|
| **Per-packet AEAD** (AES-256-GCM / ChaCha20-Poly1305) | `aead` | 32 bytes/packet | None (PSK out-of-band) | Custom protocol |
| **DTLS 1.2** (PSK / Certificate) | `dtls` | ~64 bytes/packet | Yes (standard handshake + DoS cookie) | IETF standard |
| No encryption (default) | - | 0 | - | Interops with native KCP |

> ⚠️ Do not enable both — this causes double encryption, wasting CPU and bandwidth.

### Per-packet AEAD (feature `aead`)

```rust
use std::sync::Arc;
use kcp2::crypto::{Aes256GcmCrypto, ChaCha20Poly1305Crypto};
use kcp2::{KcpConfig, KcpListener};

let key = Aes256GcmCrypto::generate_key();
let crypto = Arc::new(Aes256GcmCrypto::new(&key));

let cfg = KcpConfig::default().crypto(crypto);  // MTU automatically reduced by 32 bytes
let listener = KcpListener::bind_with_config("0.0.0.0:12345", cfg).await?;
```

Packet format: `[CONV(4) | NONCE(12) | CIPHERTEXT | AEAD_TAG(16)]`, conv kept in plaintext for Listener routing.

### DTLS 1.2 (feature `dtls`)

Full DTLS 1.2 handshake + encrypted channel, based on pure Rust [`webrtc-dtls`](https://crates.io/crates/webrtc-dtls).

**Server:**
```rust
use std::sync::Arc;
use kcp2::transport::{DtlsConfig, DtlsServerTransport};
use kcp2::{KcpConfig, KcpListener};

let dtls_cfg = DtlsConfig::server_psk(b"shared-secret".to_vec(), "kcp2");
let transport = Arc::new(DtlsServerTransport::bind("0.0.0.0:12345", dtls_cfg).await?);
let listener = KcpListener::from_transport(transport, KcpConfig::default())?;
let (conn, peer) = listener.accept().await?;
```

**Client:**
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

Full echo example:

```bash
cargo run --example dtls_echo --features dtls -- server
cargo run --example dtls_echo --features dtls -- client
```

Key points:
- DTLS default cipher suite: `TLS_PSK_WITH_AES_128_CCM_8` (IoT-friendly, only 8-byte AEAD tag).
- Server uses `DTLSListener` with built-in HelloVerifyRequest cookie, resistant to IP reflection amplification.
- KCP MTU automatically deducts `transport.overhead()` (default 64 bytes) to avoid IP fragmentation.
- Not interopable with native KCP (DTLS must be enabled on both sides).

### Custom Transport

Implement the [`KcpTransport`] trait to plug in any network stack (Unix domain sockets, QUIC, Noise tunnel, etc.):

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

## Protocol Extension

Beyond the standard KCP commands (PUSH=81, ACK=82, WASK=83, WINS=84), this implementation adds one custom extension:

### `CMD_RECONNECT` (`0x80`) — Connection Reconnect

When a client reconnects with the same `conv` after disconnection, the server may retain stale state (send buffers, receive buffers, sequence numbers). `CMD_RECONNECT` provides a clean state reset mechanism:

- **Fresh connection**: Only records the peer window — no destructive operations.
- **Reconnection** (server has existing state): Completely clears all send/recv queues, ACK lists, resets sequence numbers to 0, and restores congestion control to initial values — returning the connection to its initial state.

`CMD_RECONNECT` segment is a **24-byte header only** (no data payload), minimal network overhead.

#### Compatibility with Standard KCP

> **⚠️ `CMD_RECONNECT` is a custom extension of the `kcp2` project, NOT part of the original [skywind3000/kcp](https://github.com/skywind3000/kcp) protocol.**

- Standard KCP has only 4 commands: `PUSH(81)`, `ACK(82)`, `WASK(83)`, `WINS(84)`.
- This implementation uses command value `0x80` (128) to avoid conflict with standard commands (81-84).
- **Interoperating with native KCP implementations (C/C++/Go, etc.) will result in `InvalidCmd` errors** since they don't recognize `0x80`.
- To interoperate with standard KCP, **disable sending this command** or have the peer implement the same extension.

#### Usage (kcp2-std)

```rust
// After client connects, send CMD_RECONNECT to notify the server to reset state
session.connection().kcp().send_reconnect().await.unwrap();
```

#### Usage (kcp2-embassy)

```rust
// Call in Embassy environment
session.send_reconnect().unwrap();
```

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                        User Code                              │
├────────────────────┬─────────────────────┬───────────────────┤
│   kcp2-std          │   kcp2-embassy       │   kcp2-core        │
│   (Tokio + std)    │   (Embassy + no_std)│   (Sync, No Runtime)│
│                    │                     │                   │
│   KcpListener      │   EmbKcpSession     │   Kcp<Output>     │
│   KcpConnector     │   EmbKcpConfig      │                   │
│   KcpConnection    │   EmbassyClock      │                   │
│   KcpSession       │                     │                   │
├────────────────────┴─────────────────────┴───────────────────┤
│                    kcp2-core (Protocol Core)                   │
│             Kcp Implementation / Segment Codec / Constants    │
│         alloc_impl (Dynamic)    heapless_impl (Fixed-size)    │
├──────────────────────────────────────────────────────────────┤
│                        Transport Layer (User Provided)        │
│            tokio::net::UdpSocket | embassy-net UdpSocket      │
└──────────────────────────────────────────────────────────────┘
```

### Layer Responsibilities

| Crate | Target Platform | Async Runtime | Network Layer | Memory Model |
|-------|----------------|---------------|---------------|--------------|
| `kcp2-core` | no_std / std | None (sync) | None (output callback) | alloc or heapless |
| `kcp2-std` | std (Linux/macOS/Windows) | Tokio | `tokio::net::UdpSocket` | alloc |
| `kcp2-embassy` | no_std (ESP32) | Embassy | `embassy-net::udp::UdpSocket` | alloc |

## Quick Start

### Adding Dependencies

**std platform (default):**

```toml
[dependencies]
kcp2 = "0.2"
```

**std platform + encryption:**

```toml
[dependencies]
kcp2 = { version = "0.2", features = ["dtls"] }            # DTLS 1.2
# or
kcp2 = { version = "0.2", features = ["aead"] }            # Per-packet AEAD
```

**Embedded platform:**

```toml
[dependencies]
kcp2-core = { version = "0.1", default-features = false, features = ["alloc"] }
kcp2-embassy = "0.1"
```

**Embedded platform + AEAD encryption:**

```toml
[dependencies]
kcp2-core = { version = "0.1", default-features = false, features = ["alloc"] }
kcp2-embassy = { version = "0.1", features = ["aead"] }
```

**Core protocol only:**

```toml
[dependencies]
kcp2-core = { version = "0.1", default-features = false, features = ["heapless"] }
```

### Server (kcp2-std)

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
        println!("New connection: {} (conv: {})", addr, conn.conv());

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

### Client (kcp2-std)

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
    println!("Received: {}", String::from_utf8_lossy(&buf[..n]));

    session.close().await;
    Ok(())
}
```

### ACK Tracking

```rust
// Send and wait for acknowledgment
conn.send_and_wait_ack(b"important data").await?;

// ACK wait with timeout
conn.send_and_wait_ack_with_timeout(b"important data", Duration::from_secs(5)).await?;
```

### Embassy Async (ESP32)

```rust
use kcp2_embassy::{EmbKcpSession, EmbKcpConfig, EmbassyClock};

let config = EmbKcpConfig::embedded_constrained();

// Without encryption
let session = EmbKcpSession::new(conv, socket, remote, config);

// Or with AEAD encryption
let crypto = Some(Box::new(ChaCha20Poly1305Crypto::new(&key)) as Box<dyn EmbKcpCrypto>);
let session = EmbKcpSession::new_with_crypto(conv, socket, remote, config, crypto);

session.send(b"hello").unwrap();
let n = session.recv(&mut buf).await.unwrap();

// Or drive the update loop manually
session.step().await;
```

### Low-level API (kcp2-core)

Use the `Kcp` type directly, no async runtime or socket dependency:

```rust
use kcp2_core::Kcp;

let mut kcp = Kcp::new(conv, |data: &[u8]| {
    // Custom output callback
});

kcp.set_nodelay(true, 10, 2, true);
kcp.set_wndsize(256, 256);

kcp.send(b"hello").unwrap();
kcp.update(current_millis);

// Call when UDP data arrives
kcp.input(&recv_buf).unwrap();

let mut buf = vec![0u8; 2048];
let n = kcp.recv(&mut buf).unwrap();
```

## Feature Flags

### kcp2-core

| Feature | Deps | Description |
|---------|------|-------------|
| `std` (default) | `alloc` | Full `std` support |
| `alloc` | None | `alloc`-only, suitable for ESP32 + `esp-alloc` |
| `heapless` | `heapless` | No `alloc`, fixed-size containers (mutually exclusive with `alloc`) |
| `bytes` | `bytes` + `alloc` | `Bytes` type support |
| `fastack_conserve` | None | Fast ACK conservation mode |

### kcp2-std

| Feature | Deps | Description |
|---------|------|-------------|
| `fastack_conserve` (default) | — | Fast ACK conservation (passthrough to kcp2-core) |
| `aead` | `aes-gcm`, `chacha20poly1305`, `getrandom` | Enable per-packet AEAD (AES-256-GCM / ChaCha20-Poly1305), 32-byte overhead |
| `dtls` | `webrtc-dtls`, `webrtc-util` | Enable DTLS 1.2 encrypted channel (PSK / Certificate), ~64-byte overhead |

### kcp2-embassy

| Feature | Deps | Description |
|---------|------|-------------|
| `aead` | `aes-gcm`, `chacha20poly1305` | Enable per-packet AEAD (AES-256-GCM / ChaCha20-Poly1305), 32-byte overhead |
| `esp32c3` | — | ESP32-C3 target |
| `esp32s3` | — | ESP32-S3 target |

**Mutual exclusion**: `alloc` and `heapless` are mutually exclusive (kcp2-core). `heapless_impl` compiles only when `!alloc && heapless`.

**Feature usage across crates:**
- `kcp2-std` uses `kcp-core { features = ["std", "bytes"] }`
- `kcp2-embassy` uses `kcp-core { default-features = false, features = ["alloc"] }`

## API Reference

### KcpListener (Server, kcp2-std)

| Method | Description |
|--------|-------------|
| `bind(addr)` | Bind to address with default config |
| `bind_with_config(addr, config)` | Bind to address with custom config |
| `from_socket(socket, config)` | Use external UdpSocket |
| `from_transport(transport, config)` | Use custom transport implementation (e.g., DTLS) |
| `accept()` | Accept a new connection, returns `(KcpConnection, SocketAddr)` |
| `recv_from(buf)` | Receive data, returns `(size, KcpConnection, addr)` |
| `create_connection(conv, addr)` | Manually create a connection |
| `get_connection(conv)` | Find connection by conv |
| `remove_connection(conv)` | Remove a connection |
| `connection_count()` | Current number of connections |
| `local_addr()` | Get local bound address |
| `close()` | Close the listener |

### KcpConnector (Client, kcp2-std)

| Method | Description |
|--------|-------------|
| `new(addr)` | Create a connector |
| `from_socket(socket, addr, config)` | Use external UdpSocket |
| `from_transport(transport, addr, config)` | Use custom transport implementation |
| `.conv(v)` / `.set_conv(v)` | Set session ID |
| `.nodelay(...)` / `.set_nodelay(...)` | Configure nodelay |
| `.wndsize(...)` / `.set_wndsize(...)` | Configure window size |
| `.timeout(d)` / `.set_timeout(d)` | Set timeout |
| `.connect()` | Establish connection, returns `KcpSession` |
| `.connect_with_handles()` | Also returns task JoinHandle |

### KcpSession (Client Session, kcp2-std)

| Method | Description |
|--------|-------------|
| `connection()` | Get underlying `KcpConnection` |
| `close()` | Close session, stop background tasks |
| `is_alive()` | Check if connection is alive |
| `is_closed()` | Check if session is closed |

### KcpConnection (Connection Abstraction, kcp2-std)

| Method | Description |
|--------|-------------|
| `send(data)` | Send data |
| `recv(buf)` | Receive data (blocking) |
| `try_recv(buf)` | Non-blocking receive |
| `send_and_wait_ack(data)` | Send and wait for ACK |
| `send_and_wait_ack_with_timeout(data, timeout)` | ACK wait with timeout |
| `wait_all_sent()` | Wait for all data to be sent |
| `is_dead()` | Check if connection is dead |
| `close()` | Close connection |

### EmbKcpConfig (kcp2-embassy)

**Preset configurations:**

| Preset Method | Scenario |
|---------------|----------|
| `default()` | General-purpose |
| `high_latency()` | High-latency networks |
| `high_loss()` | High packet loss networks |
| `low_latency()` | Low-latency scenarios |
| `embedded_constrained()` | Resource-constrained embedded devices |

**Builder methods:** `nodelay()`, `wndsize()`, `mtu()`, `timeout_ms()`

### AsyncKcp (Internal Actor Wrapper, kcp2-std)

`AsyncKcp` is the internal Actor wrapper in kcp2-std that wraps a synchronous `Kcp` instance into an async interface. Generally not needed directly — `KcpListener` and `KcpConnector` provide the full API.

## Configuration Parameters

### KcpConfig Fields

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `nodelay` | `bool` | `false` | Enable nodelay mode, reduces RTO |
| `interval` | `u32` | `100` | Internal clock interval (ms) |
| `resend` | `u32` | `0` | Fast retransmit threshold (0=disabled) |
| `nc` | `bool` | `false` | Disable congestion control |
| `sndwnd` | `u16` | `32` | Send window size |
| `rcvwnd` | `u16` | `128` | Receive window size |
| `mtu` | `usize` | `1400` | Maximum transmission unit |
| `rx_minrto` | `u32` | `100` | Minimum RTO (ms) |
| `dead_link` | `u32` | `10` | Max retransmits before dead link |
| `stream` | `bool` | `false` | Stream mode |
| `timeout` | `Duration` | `30s` | Connection timeout |
| `crypto()` | builder | `None` | Inject `KcpCrypto` implementation (feature `aead`), auto-deducts MTU overhead |

### Scenario Configuration Recommendations

**High-latency networks** (satellite links, cross-continent connections):

```rust
KcpConfig::default()
    .nodelay(true, 150, 2, false)
    .wndsize(512, 512)
    .rx_minrto(300)
    .dead_link(18)
```

**High packet loss networks** (wireless, mobile):

```rust
KcpConfig::default()
    .nodelay(true, 80, 1, true)
    .wndsize(256, 256)
    .rx_minrto(80)
    .dead_link(10)
```

**Low-latency scenarios** (LAN, same-city):

```rust
KcpConfig::default()
    .nodelay(true, 10, 2, true)
    .wndsize(512, 512)
    .rx_minrto(30)
    .dead_link(8)
```

## Important Usage Notes

### Send Size Limits

`send()` splits data into KCP segments. The maximum number of segments per call is bounded by the receive window size (`WND_RCV`, default 128). When `stream` mode is enabled and data exceeds `WND_RCV × MSS` (~176KB with defaults), `send()` returns `Err(TooManyFragments)` instead of silently delivering partial data.

```rust
// WRONG: may silently lose data in older versions, now returns error
conn.send(&huge_data).await.unwrap();  // panics if oversized

// RIGHT: handle the error explicitly
match conn.send(&huge_data).await {
    Ok(()) => { /* sent */ }
    Err(KcpError::TooManyFragments { .. }) => { /* chunk and retry */ }
    Err(e) => return Err(e.into()),
}
```

**Recommendation**: For large transfers, chunk data into sizes well below `WND_RCV × MSS` at the application layer.

### Receive Buffer Must Be Large Enough

`recv()` / `try_recv()` returns `Err(BufferTooSmall { required, available })` when the provided buffer is too small for the next message. **Data is not consumed** — you can retry with a larger buffer.

```rust
let mut buf = vec![0u8; 2048];
loop {
    match conn.recv(&mut buf).await {
        Ok(n) => { /* process &buf[..n] */ }
        Err(KcpError::BufferTooSmall { required, .. }) => {
            buf.resize(required, 0);
            continue;  // retry with larger buffer
        }
        Err(e) => break,
    }
}
```

### Never Ignore Error Return Values

All I/O methods (`send`, `recv`, `input`, `flush`) may return errors that indicate data loss or connection failure. Ignoring these with `let _ =` or `.unwrap()` in non-test code can mask bugs:

```rust
// WRONG: silently drops send failures
let _ = session.send(b"important data");

// RIGHT: handle or propagate
session.send(b"important data")?;
// or at minimum, log it
if let Err(e) = session.send(b"important data") {
    log::error!("send failed: {e}");
}
```

This is especially important for `kcp2-embassy` where the `step()` method drives both `input()` and `flush()` internally — ensure your `log` backend is initialized to capture warnings.

### Stream Mode Boundary Behavior

When `stream` mode is enabled (`KcpConfig::stream(true)`), KCP coalesces consecutive `send()` calls into shared segments for efficiency. This means:

- Message boundaries are **not preserved** — two 100-byte sends may arrive as one 200-byte recv, or be split differently.
- If you need message framing, implement it at the application layer (e.g., length-prefix protocol).

## Error Types

| Variant | Description |
|---------|-------------|
| `ConvMismatch` | Session ID mismatch |
| `InvalidCmd` | Invalid command |
| `RecvQueueEmpty` | Receive queue empty |
| `IncompletePacket` | Incomplete packet |
| `DeadLink` | Connection is dead |
| `Timeout` | Operation timed out |
| `BufferTooSmall` | Insufficient buffer size |
| `TooManyFragments` | Data too large for single send (exceeds `WND_RCV × MSS`) |

## Examples

### std Platform

| Example | Description | Run |
|---------|-------------|-----|
| `echo` | Low-level KCP loopback | `cargo run --example echo` |
| `high_level_api` | KcpListener + KcpConnector Echo service | `cargo run --example high_level_api server` |
| `heartbeat` | Heartbeat + disconnect reconnect + exponential backoff | `cargo run --example heartbeat` |
| `multi_server` | Multi-connection server | `cargo run --example multi_server server` |
| `udp_echo` | UDP Echo baseline | `cargo run --example udp_echo` |
| `performance_test` | Performance benchmark | `cargo run --example performance_test` |
| `dtls_echo` | KCP over DTLS 1.2 (PSK) | `cargo run --example dtls_echo --features dtls -- server` |

### ESP32 Platform

| Example | Target Chip | Description | Build |
|---------|-------------|-------------|-------|
| `embassy-esp32` | ESP32-C3 / S3 | Embassy async KCP Echo | `./build.sh --chip c3` |
| `embassy-esp32-heartbeat` | ESP32-C3 / S3 | KCP heartbeat + disconnect detection + auto reconnect | `./build.sh --chip c3` |
| `embassy-esp32-heartbeat` | ESP32-C3 / S3 | Heartbeat + AEAD encryption (ChaCha20) | `./build.sh --chip c3 --aead` |
| `embassy-esp32-heartbeat` | ESP32-C3 / S3 | Heartbeat + AEAD encryption (AES-256-GCM) | `./build.sh --chip c3 --aes` |

## Dependencies

### kcp2-core

| Library | Purpose |
|---------|---------|
| `log` | Logging (no_std compatible) |
| `heapless` | Optional, fixed-size containers |
| `bytes` | Optional, `Bytes` type support |

### kcp2-std

| Library | Purpose |
|---------|---------|
| `kcp2-core` | Protocol core (std + bytes features) |
| `tokio` | Async runtime |
| `bytes` | Zero-copy buffering |
| `dashmap` | Concurrent HashMap (connection table) |
| `crossbeam-queue` | Lock-free queue (buffer pool) |
| `parking_lot` | High-performance Mutex |
| `thiserror` | Error type derivation |
| `futures-util` | Async utilities |
| `log` | Logging |
| `aes-gcm` / `chacha20poly1305` | Per-packet AEAD (`aead` feature only) |
| `webrtc-dtls` / `webrtc-util` | DTLS 1.2 encrypted channel (`dtls` feature only) |

### kcp2-embassy

| Library | Purpose |
|---------|---------|
| `kcp2-core` | Protocol core (alloc feature) |
| `embassy-executor` | Embassy async executor |
| `embassy-time` | Embassy timer |
| `embassy-net` | Embassy network stack |
| `embassy-sync` | Embassy synchronization primitives |
| `embassy-futures` | Embassy async utilities |
| `heapless` | Fixed-size containers |
| `static_cell` | Static allocation |
| `log` | Logging |
| `aes-gcm` / `chacha20poly1305` | Per-packet AEAD (`aead` feature only) |

## Benchmarks

```bash
cargo bench
```

Contains two benchmark groups:

- `kcp_benchmark`: Protocol layer benchmarks — tests `send`/`recv`/`input`/`update` performance
- `listener_benchmark`: Listener layer benchmarks — tests multi-connection throughput

## QA Script

```bash
./qa.sh
```

Runs 7 checks:

1. `cargo check`: Verifies compilation across 10 feature combinations
2. `cargo clippy`: Code style checks
3. `cargo test`: Unit and integration tests
4. `cargo test --doc`: Doc tests
5. `cargo run --example`: Compiles and runs all examples
6. `cargo bench`: Verifies benchmark compilation
7. `cargo check -p kcp2-core --no-default-features`: Checks no_std compatibility

## License

MIT

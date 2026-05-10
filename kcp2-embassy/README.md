# kcp2-embassy

KCP protocol Embassy async layer for ESP32 and `no_std` devices.

Built on top of [kcp2-core](../kcp2-core), this crate provides an async KCP session implementation using the [Embassy](https://embassy.dev/) executor and networking stack, targeting embedded platforms like ESP32.

## Quick Usage

```rust
use kcp2_embassy::{EmbKcpSession, EmbKcpConfig};

// Create a KCP session with preset configuration
let config = EmbKcpConfig::embedded_constrained();
let mut session = EmbKcpSession::new(
    conv_id,
    udp_socket,          // embassy_net::udp::UdpSocket
    remote_endpoint,     // IpEndpoint
    config,
);

// Send data
session.send(b"hello")?;

// Receive data
let n = session.recv(&mut buf).await?;

// Drive the KCP update loop
session.step().await;
```

## Preset Configurations

| Preset | Description |
|--------|-------------|
| `EmbKcpConfig::default()` | General-purpose |
| `EmbKcpConfig::high_latency()` | Satellite / cross-region links |
| `EmbKcpConfig::high_loss()` | Wireless / mobile networks |
| `EmbKcpConfig::low_latency()` | LAN / same-city |
| `EmbKcpConfig::embedded_constrained()` | Memory-constrained ESP32 |

## Cargo Features

| Feature | Description |
|---------|-------------|
| `esp32c3` | ESP32-C3 target support |
| `esp32s3` | ESP32-S3 target support |

## License

MIT

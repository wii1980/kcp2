# kcp2-std

KCP protocol async layer for std platforms — Tokio-based Actor model with `KcpListener` / `KcpConnector`.

Built on top of [kcp2-core](../kcp2-core), this crate provides a full-featured async KCP implementation for Linux, macOS, and Windows.

## Quick Start

```rust
use kcp2_std::{KcpConfig, KcpListener, KcpConnector};
use tokio::time::Duration;

// Server
let config = KcpConfig::default().wndsize(512, 512);
let listener = KcpListener::bind_with_config("0.0.0.0:12345", config).await?;

// Client
let session = KcpConnector::new("127.0.0.1:12345")?
    .with_config(KcpConfig::default())
    .conv(1)
    .connect()
    .await?;
```

## Features

| Feature | Description |
|---------|-------------|
| `fastack_conserve` (default) | Fast ACK conservation |
| `aead` | Per-packet AEAD encryption (AES-256-GCM / ChaCha20-Poly1305) |
| `dtls` | DTLS 1.2 encrypted channel (PSK / Certificate) |

## Cargo Features

```toml
[dependencies]
kcp2-std = "0.1"                              # no encryption (default)
kcp2-std = { version = "0.1", features = ["aead"] }   # per-packet AEAD
kcp2-std = { version = "0.1", features = ["dtls"] }   # DTLS 1.2
```

## License

MIT

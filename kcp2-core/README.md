# kcp2-core

KCP protocol core implementation — `no_std` compatible, zero-dependency network layer.

This crate provides the pure algorithmic KCP protocol control block with no OS or network layer dependencies. It can be used in embedded, `no_std`, and standard environments alike.

## Protocol Extension

This implementation adds one custom command beyond the standard KCP protocol (PUSH=81, ACK=82, WASK=83, WINS=84):

### `CMD_RECONNECT` (`0x80`) — Connection Reconnect

When a client reconnects with the same `conv` after disconnection, the server may retain stale state (send buffers, receive buffers, sequence numbers). `CMD_RECONNECT` provides a clean state reset mechanism:

- **Fresh connection**: Only records the peer window, no destructive operations.
- **Reconnection** (server has existing state): Clears all send/recv queues, resets sequence numbers to 0, restores congestion control to initial values — effectively a "soft reset".

The segment is **24 bytes header-only** (no data payload), minimal network overhead.

> **⚠️ Compatibility:** `CMD_RECONNECT` is a `kcp2`-specific extension, **NOT** part of the standard [skywind3000/kcp](https://github.com/skywind3000/kcp) protocol. The value `0x80` (128) avoids conflict with standard commands (81-84). Interoperating with native KCP implementations (C/C++/Go) will result in `InvalidCmd` errors.

## Features

| Feature | Default | Description |
|---|---|---|
| `std` | ✅ | Enables `std` support (implies `alloc`), provides a built-in `Clock` implementation |
| `alloc` | — | Enables `alloc`-based `Kcp` (dynamic buffer sizes) |
| `heapless` | — | Enables `heapless`-based `Kcp` (fixed-size, no `alloc` needed) |
| `bytes` | — | Enables `input_bytes()` accepting `bytes::Bytes`; implies `alloc` |
| `fastack_conserve` | ✅ | Conservative fast retransmit — reduces spurious retransmissions |

> **Note:** `alloc` and `heapless` are mutually exclusive. Only one `Kcp` variant is available at a time.

## Quick Start

```rust
use kcp2_core::{Kcp, KcpOutput, Clock};

struct MyClock;
impl Clock for MyClock {
    fn now_ms(&self) -> u32 {
        // Return current time in milliseconds
        0
    }
}

fn main() {
    let output = KcpOutput::new(|data, conv| {
        // Send `data` over your transport (e.g. UDP socket)
        println!("sending {} bytes for conv {}", data.len(), conv);
    });

    let mut kcp = Kcp::new(42, output);
    kcp.set_nodelay(true, 10, 0, true);
    kcp.set_wndsize(256, 256);

    // User → KCP: application data to send
    kcp.send(b"hello world").unwrap();

    // Network → KCP: incoming KCP packets
    // kcp.input(&incoming_bytes).unwrap();

    // KCP → User: received application data
    let mut buf = [0u8; 1500];
    // let n = kcp.recv(&mut buf).unwrap();

    // Call update regularly (e.g. every 10ms)
    let now = 0; // your clock value
    kcp.update(now);
}
```

## API Overview

- **`Kcp::new(conv, output)`** — Create a KCP session with conversation ID and output callback
- **`send(data)`** / **`recv(buf)`** — Application-level send and receive
- **`input(data)`** — Feed incoming KCP packets from the network
- **`update(current)`** — Drive the internal timer (call regularly, e.g. every 10ms)
- **`check(current)`** — Query the next time `update` should be called

For the full list of configuration options see [`set_nodelay`](src/kcp/alloc_impl.rs), [`set_wndsize`](src/kcp/alloc_impl.rs), [`set_mtu`](src/kcp/alloc_impl.rs), etc.

## Related Crates

- **[rs-kcp2](https://github.com/wii1980/kcp2)** — Full async KCP implementation with sockets (`kcp2-std`, `kcp2-embassy`)

## License

MIT

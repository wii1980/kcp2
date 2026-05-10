#![allow(
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::let_and_return
)]

//! KCP 协议 Embassy 异步层 — 用于 ESP32 等 no_std 设备
//!
//! # 协议扩展
//!
//! 底层 `kcp2-core` 在标准 KCP 命令之外增加了一个自定义扩展命令：
//! **`CMD_RECONNECT`** (`0x80`) — 连接重连指令，用于断线后重置对端状态。
//! 详见 `kcp2-core` 文档。
//!
//! 基于 embassy-net 的 UDP 传输，配合 kcp-core 核心协议。
//!
//! # 架构
//!
//! ```text
//! ┌────────────────────────────────────┐
//! │          用户应用                   │
//! ├────────────────────────────────────┤
//! │     EmbKcpSession                  │
//! │     (embassy timer + KCP core)     │
//! ├────────────────────────────────────┤
//! │     embassy-net::udp::UdpSocket    │
//! ├────────────────────────────────────┤
//! │     embassy-net (smoltcp)          │
//! ├────────────────────────────────────┤
//! │     esp-wifi / esp-radio           │
//! └────────────────────────────────────┘
//! ```

#![no_std]

extern crate alloc;

use embassy_time::Instant;
use kcp2_core::Clock;

pub mod config;
pub mod crypto;
mod session;

pub use config::EmbKcpConfig;
pub use crypto::EmbKcpCrypto;
pub use session::EmbKcpSession;

/// Embassy 时钟实现 — 使用 embassy_time 提供毫秒时间戳
pub struct EmbassyClock;

impl Clock for EmbassyClock {
    fn now_ms(&self) -> u32 {
        Instant::now().as_millis() as u32
    }
}

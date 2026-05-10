//! KCP 协议核心实现 — `no_std` 兼容
//!
//! 提供纯算法的 KCP 协议控制块，不依赖任何操作系统或网络层。
//!
//! # 协议扩展
//!
//! 本实现在标准 KCP 协议命令（PUSH=81, ACK=82, WASK=83, WINS=84）之外，
//! 增加了一个自定义扩展命令：
//!
//! - **`CMD_RECONNECT`** (`0x80`) — 连接重连指令。
//!   当客户端断线后以相同 `conv` 重连时，用于通知服务端重置过期状态。
//!   段仅含 24 字节头部（无数据 payload），接收方会清空所有缓冲并重置序列号。
//!
//! ## 与标准 KCP 的兼容性
//!
//! `CMD_RECONNECT` 是 `kcp2` 项目的自定义扩展，**不是**标准 KCP 协议的一部分。
//! 与原生 KCP 实现（C/C++/Go 等）互操作时，对端不认识此命令值 `0x80`，
//! 会返回 `InvalidCmd` 错误。如需与标准 KCP 互操作，应禁用此命令的发送，
//! 或使对端也实现相应扩展。

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::wildcard_imports,
    clippy::map_unwrap_or,
    clippy::manual_let_else,
    clippy::needless_pass_by_value,
    clippy::uninlined_format_args,
    clippy::similar_names
)]

#[cfg(feature = "alloc")]
extern crate alloc;

mod consts;
mod errors;
mod kcp;
mod segment;

pub use errors::{KcpError, Result};
#[cfg(any(feature = "alloc", feature = "heapless"))]
pub use kcp::{Kcp, KcpOutput, LinkState, SendHandle};
pub use segment::Segment;

pub trait Clock {
    fn now_ms(&self) -> u32;
}

#[cfg(feature = "std")]
mod std_clock {
    use super::Clock;
    use std::sync::OnceLock;
    use std::time::Instant;

    static START: OnceLock<Instant> = OnceLock::new();

    pub struct StdClock;

    impl Clock for StdClock {
        fn now_ms(&self) -> u32 {
            let start = START.get_or_init(Instant::now);
            start.elapsed().as_millis() as u32
        }
    }

    static CLOCK: OnceLock<StdClock> = OnceLock::new();

    pub fn current() -> u32 {
        CLOCK.get_or_init(|| StdClock).now_ms()
    }
}

#[cfg(feature = "std")]
pub use std_clock::current;

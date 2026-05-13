#![allow(clippy::module_name_repetitions)]

//! KCP 协议 std/async 层 — 基于 Tokio Actor 模式
//!
//! # 协议扩展
//!
//! 底层 `kcp2-core` 在标准 KCP 命令之外增加了一个自定义扩展命令：
//! **`CMD_RECONNECT`** (`0x80`) — 连接重连指令，用于断线后重置对端状态。
//! 详见 `kcp2-core` 文档。
//!
//! 重新导出 kcp-core 的核心类型，并添加：
//! - AsyncKcp: Actor 模式异步封装
//! - KcpListener: 服务端监听器
//! - KcpConnector/KcpSession: 客户端连接器
//! - KcpConnection: 连接抽象
//! - KcpConfig: 配置

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::return_self_not_must_use,
    clippy::similar_names,
    clippy::uninlined_format_args,
    clippy::wildcard_imports,
    clippy::ignored_unit_patterns,
    clippy::unnested_or_patterns,
    clippy::semicolon_if_nothing_returned,
    clippy::let_and_return,
    clippy::single_match_else,
    clippy::needless_continue,
    clippy::unused_async
)]

pub use kcp2_core::{
    current, Kcp, KcpError, KcpOutput, Result, Segment, SendHandle, Clock,
};

pub mod crypto;
pub mod transport;

mod async_kcp;
mod buffer_pool;
mod config;
mod connection;
mod connector;
mod conv_generator;
mod listener;
mod reaper;

pub use async_kcp::AsyncKcp;
pub use config::KcpConfig;
pub use connection::KcpConnection;
pub use connector::{KcpConnector, KcpSession};
pub use conv_generator::ConvGenerator;
pub use listener::KcpListener;

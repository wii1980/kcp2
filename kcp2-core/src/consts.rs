/// KCP 连接重连指令（自定义扩展，非标准 KCP 协议命令）。
///
/// ## 功能
///
/// `CMD_RECONNECT` (0x80) 用于在同一条 KCP 连接 (`conv`) 上触发**连接状态重置**。
/// 当客户端断线后以相同 `conv` 重连时，服务端可能残留过期状态（发送缓冲、接收缓冲、
/// 序列号等），CMD_RECONNECT 提供了一种清洁的状态重置机制：
///
/// - **全新连接**（`rcv_nxt == 0 && snd_nxt == 0`）：仅记录对端窗口，标记连接为已初始化。
/// - **重连**（已有有效状态）：**完全清空**所有发送/接收队列、ACK 列表、
///   重置序列号为 0、恢复拥塞控制初始值，使连接回到初始状态。
///
/// CMD_RECONNECT 段为纯头部（24 字节），不携带数据 payload。
///
/// ## 与标准 KCP 协议的兼容性
///
/// **此命令是 `kcp2` 项目的自定义扩展，不属于 [skywind3000/kcp](https://github.com/skywind3000/kcp)
/// 原版协议。**
///
/// - 标准 KCP 仅有 4 个命令：PUSH(81)、ACK(82)、WASK(83)、WINS(84)。
/// - 此实现使用命令值 `0x80` (128) 以避免与标准命令（81-84）冲突。
/// - **与原生 KCP 实现（C/C++/Go 等）互操作时，对方会返回 `InvalidCmd` 错误**，
///   因为 `0x80` 不在对方的命令白名单中。
/// - 如需与标准 KCP 互操作，应**禁用 CMD_RECONNECT** 发送，或使对端也实现相应扩展。
pub(crate) const CMD_RECONNECT: u8 = 0x80;
pub(crate) const CMD_PUSH: u8 = 81;
pub(crate) const CMD_ACK: u8 = 82;
pub(crate) const CMD_WASK: u8 = 83;
pub(crate) const CMD_WINS: u8 = 84;

pub(crate) const ASK_SEND: u32 = 1;
pub(crate) const ASK_TELL: u32 = 2;

pub(crate) const RTO_NDL: u32 = 30;
pub(crate) const RTO_DEF: u32 = 200;
pub(crate) const RTO_MIN: u32 = 100;
pub(crate) const RTO_MAX: u32 = 60000;
pub(crate) const WND_SND: u16 = 32;
pub(crate) const WND_RCV: u16 = 128;
pub(crate) const MTU_DEF: usize = 1400;
pub(crate) const INTERVAL: u32 = 100;
pub(crate) const OVERHEAD: usize = 24;
pub(crate) const DEADLINK: u32 = 20;
pub(crate) const THRESH_INIT: u16 = 2;
pub(crate) const THRESH_MIN: u16 = 2;
pub(crate) const PROBE_INIT: u32 = 7000;
pub(crate) const PROBE_LIMIT: u32 = 120_000;
pub(crate) const FASTACK_LIMIT: u32 = 5;

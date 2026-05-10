//! 可插拔的 KCP 传输层抽象
//!
//! `KcpTransport` trait 将网络 I/O 从 `tokio::net::UdpSocket` 解耦，
//! 使 KCP 可以运行在任何实现了该 trait 的传输层之上（UDP、DTLS、自定义等）。
//!
//! `UdpTransport` 是默认实现，直接包装 `tokio::net::UdpSocket`，零开销。
//!
//! 加密层 `KcpCrypto` 与此 trait 正交 — 两者在 `KcpActor` 中独立配置。
//!
//! 启用 `dtls` feature 后，可使用 [`DtlsClientTransport`] / [`DtlsServerTransport`]
//! 在 UDP 之上提供完整 DTLS 1.2 加密通道。

#[cfg(feature = "dtls")]
pub mod dtls;

#[cfg(feature = "dtls")]
pub use dtls::{DtlsClientTransport, DtlsConfig, DtlsServerTransport, DEFAULT_DTLS_OVERHEAD};

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use tokio::net::UdpSocket;

/// 接收操作的 Future 类型
type RecvFuture<'a> = Pin<Box<dyn Future<Output = io::Result<usize>> + Send + 'a>>;
/// 接收数据及来源地址的 Future 类型
type RecvFromFuture<'a> = Pin<Box<dyn Future<Output = io::Result<(usize, SocketAddr)>> + Send + 'a>>;

/// KCP 传输层 trait
///
/// 抽象 UDP socket 的发送（同步非阻塞）和接收（异步）操作。
/// 实现必须 Send + Sync 以支持跨 task 共享。
pub trait KcpTransport: Send + Sync {
    /// 发送数据（已连接模式，同步非阻塞）
    fn try_send(&self, buf: &[u8]) -> io::Result<usize>;

    /// 发送数据到指定地址（同步非阻塞）
    fn try_send_to(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize>;

    /// 接收数据（已连接模式，异步）
    fn recv<'a>(&'a self, buf: &'a mut [u8]) -> RecvFuture<'a>;

    /// 接收数据及来源地址（异步）
    fn recv_from<'a>(&'a self, buf: &'a mut [u8]) -> RecvFromFuture<'a>;

    /// 本地地址
    fn local_addr(&self) -> io::Result<SocketAddr>;

    /// 传输层 overhead（用于 MTU 自动调整）
    fn overhead(&self) -> usize {
        0
    }
}

/// UDP 传输层 — `tokio::net::UdpSocket` 的零开销封装
pub struct UdpTransport {
    socket: Arc<UdpSocket>,
}

impl UdpTransport {
    pub fn new(socket: UdpSocket) -> Self {
        Self {
            socket: Arc::new(socket),
        }
    }

    pub fn from_arc(socket: Arc<UdpSocket>) -> Self {
        Self { socket }
    }

    pub fn inner(&self) -> &Arc<UdpSocket> {
        &self.socket
    }
}

impl KcpTransport for UdpTransport {
    fn try_send(&self, buf: &[u8]) -> io::Result<usize> {
        self.socket.try_send(buf)
    }

    fn try_send_to(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize> {
        self.socket.try_send_to(buf, target)
    }

    fn recv<'a>(&'a self, buf: &'a mut [u8]) -> RecvFuture<'a> {
        Box::pin(self.socket.recv(buf))
    }

    fn recv_from<'a>(&'a self, buf: &'a mut [u8]) -> RecvFromFuture<'a> {
        Box::pin(self.socket.recv_from(buf))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}

impl std::fmt::Debug for UdpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UdpTransport")
            .field("local_addr", &self.socket.local_addr().ok())
            .finish()
    }
}

impl<T: KcpTransport + ?Sized> KcpTransport for Arc<T> {
    fn try_send(&self, buf: &[u8]) -> io::Result<usize> {
        (**self).try_send(buf)
    }

    fn try_send_to(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize> {
        (**self).try_send_to(buf, target)
    }

    fn recv<'a>(&'a self, buf: &'a mut [u8]) -> RecvFuture<'a> {
        (**self).recv(buf)
    }

    fn recv_from<'a>(&'a self, buf: &'a mut [u8]) -> RecvFromFuture<'a> {
        (**self).recv_from(buf)
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        (**self).local_addr()
    }

    fn overhead(&self) -> usize {
        (**self).overhead()
    }
}

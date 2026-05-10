//! DTLS 1.2 加密传输层
//!
//! 在 UDP 之上提供完整的 DTLS 1.2 握手与加密通道，实现透明的机密性、完整性、
//! 抗重放与对端鉴权。适用于公网传输、IoT 设备遥测、企业 PKI 集成等场景。
//!
//! 启用本模块需要 `dtls` cargo feature：
//!
//! ```toml
//! kcp2-std = { version = "0.2", features = ["dtls"] }
//! ```
//!
//! 实现基于 [`webrtc-dtls`](https://crates.io/crates/webrtc-dtls)（纯 Rust，DTLS 1.2，
//! 无需 OpenSSL）和 [`webrtc-util`](https://crates.io/crates/webrtc-util)。
//!
//! # 示例：PSK 客户端
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use kcp2_std::transport::{DtlsClientTransport, DtlsConfig};
//! use kcp2_std::{KcpConnector, KcpConfig};
//!
//! let cfg = DtlsConfig::client_psk(b"shared-secret".to_vec(), b"kcp2");
//! let transport = Arc::new(DtlsClientTransport::connect("127.0.0.1:12345", cfg).await?);
//! let session = KcpConnector::from_transport(transport, "127.0.0.1:12345", KcpConfig::default())?
//!     .conv(1)
//!     .connect()
//!     .await?;
//! ```
//!
//! # 示例：PSK 服务端
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use kcp2_std::transport::{DtlsServerTransport, DtlsConfig};
//! use kcp2_std::{KcpListener, KcpConfig};
//!
//! let cfg = DtlsConfig::server_psk(b"shared-secret".to_vec(), b"kcp2");
//! let transport = Arc::new(DtlsServerTransport::bind("0.0.0.0:12345", cfg).await?);
//! let listener = KcpListener::from_transport(transport, KcpConfig::default())?;
//! ```
//!
//! # 与整包 AEAD（`aead` feature）的互斥
//!
//! DTLS 已提供完整加密层，**不要同时启用 `KcpConfig::crypto(...)`**——会双重加密，
//! 浪费 CPU 和带宽。两种方案选其一：
//!
//! - DTLS：标准协议，PKI/PSK 友好，多 ~64 字节 overhead
//! - 整包 AEAD：自定义协议，无握手（需带外分发密钥），32 字节 overhead

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex as TokioMutex, mpsc};
use tokio::task::JoinHandle;
use webrtc_dtls::conn::DTLSConn;
use webrtc_util::conn::{Conn as UtilConn, Listener as UtilListener};

pub use webrtc_dtls::cipher_suite::CipherSuiteId;
pub use webrtc_dtls::config::{ClientAuthType, Config as DtlsRawConfig, ExtendedMasterSecretType};
pub use webrtc_dtls::crypto::Certificate as DtlsCertificate;

use super::KcpTransport;

/// DTLS 协议默认 overhead（用于 KCP MTU 自动调整）
///
/// 包含：
/// - DTLS Record Header：13 字节
/// - 显式 IV / Nonce：最多 16 字节
/// - AEAD Tag：最多 16 字节
/// - 余量（非 AEAD 套件 padding 等）：约 19 字节
///
/// 合计 64 字节，覆盖 webrtc-dtls 当前所有支持的密码套件。
/// 如已固定使用某 cipher suite，可通过 [`DtlsConfig::overhead`] 收紧。
pub const DEFAULT_DTLS_OVERHEAD: usize = 64;

/// 默认握手超时（秒）
const DEFAULT_HANDSHAKE_TIMEOUT_SECS: u64 = 10;

/// 默认每会话出站队列大小
const DEFAULT_SEND_QUEUE_SIZE: usize = 256;

/// 默认每会话入站缓冲区大小
const DEFAULT_RECV_BUF_SIZE: usize = 4096;

/// DTLS 配置（客户端 / 服务端共用）
///
/// 内部委托给 [`webrtc_dtls::config::Config`]，并补充 KCP 层关心的参数：
/// 握手超时、协议 overhead、队列容量。
#[derive(Clone)]
pub struct DtlsConfig {
    /// 底层 webrtc-dtls 原始配置
    pub inner: DtlsRawConfig,
    /// 单次握手超时
    pub handshake_timeout: Duration,
    /// 自报告的协议 overhead（KCP MTU 会扣除该值）
    pub overhead: usize,
    /// 出站发送队列大小（每个 DTLS 会话独立维护）
    pub send_queue_size: usize,
    /// 入站缓冲区单包最大字节
    pub recv_buf_size: usize,
}

impl Default for DtlsConfig {
    fn default() -> Self {
        Self {
            inner: DtlsRawConfig::default(),
            handshake_timeout: Duration::from_secs(DEFAULT_HANDSHAKE_TIMEOUT_SECS),
            overhead: DEFAULT_DTLS_OVERHEAD,
            send_queue_size: DEFAULT_SEND_QUEUE_SIZE,
            recv_buf_size: DEFAULT_RECV_BUF_SIZE,
        }
    }
}

impl DtlsConfig {
    /// 创建客户端 PSK 配置
    ///
    /// 使用 `TLS_PSK_WITH_AES_128_CCM_8`，IoT 友好（资源占用低），仅 8 字节 AEAD tag。
    pub fn client_psk(psk: Vec<u8>, identity_hint: impl Into<Vec<u8>>) -> Self {
        let psk_clone = psk.clone();
        let inner = DtlsRawConfig {
            psk: Some(Arc::new(move |_hint: &[u8]| Ok(psk_clone.clone()))),
            psk_identity_hint: Some(identity_hint.into()),
            cipher_suites: vec![CipherSuiteId::Tls_Psk_With_Aes_128_Ccm_8],
            extended_master_secret: ExtendedMasterSecretType::Require,
            ..DtlsRawConfig::default()
        };
        Self {
            inner,
            ..Self::default()
        }
    }

    /// 创建服务端 PSK 配置
    pub fn server_psk(psk: Vec<u8>, identity_hint: impl Into<Vec<u8>>) -> Self {
        let psk_clone = psk.clone();
        let inner = DtlsRawConfig {
            psk: Some(Arc::new(move |_hint: &[u8]| Ok(psk_clone.clone()))),
            psk_identity_hint: Some(identity_hint.into()),
            cipher_suites: vec![CipherSuiteId::Tls_Psk_With_Aes_128_Ccm_8],
            extended_master_secret: ExtendedMasterSecretType::Require,
            ..DtlsRawConfig::default()
        };
        Self {
            inner,
            ..Self::default()
        }
    }

    /// 设置握手超时
    pub fn handshake_timeout(mut self, t: Duration) -> Self {
        self.handshake_timeout = t;
        self
    }

    /// 自定义协议 overhead（默认 64）
    pub fn overhead(mut self, n: usize) -> Self {
        self.overhead = n;
        self
    }

    /// 自定义出站队列大小（默认 256）
    pub fn send_queue_size(mut self, n: usize) -> Self {
        self.send_queue_size = n.max(1);
        self
    }

    /// 自定义入站单包缓冲（默认 4096）
    pub fn recv_buf_size(mut self, n: usize) -> Self {
        self.recv_buf_size = n.max(512);
        self
    }
}

impl std::fmt::Debug for DtlsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DtlsConfig")
            .field("handshake_timeout", &self.handshake_timeout)
            .field("overhead", &self.overhead)
            .field("send_queue_size", &self.send_queue_size)
            .field("recv_buf_size", &self.recv_buf_size)
            .field("cipher_suites", &self.inner.cipher_suites)
            .finish()
    }
}

// ─── 客户端 ─────────────────────────────────────────────────────

/// DTLS 客户端传输 — 单会话长连接
///
/// 构造时同步完成 DTLS 握手；后续 `try_send` 通过内部队列异步加密发送，
/// `recv` 从内部队列拉取已解密明文。
pub struct DtlsClientTransport {
    send_tx: mpsc::Sender<Vec<u8>>,
    decrypted_rx: TokioMutex<mpsc::Receiver<Vec<u8>>>,
    local_addr: SocketAddr,
    remote_addr: SocketAddr,
    overhead: usize,
    closed: Arc<AtomicBool>,
    send_task: JoinHandle<()>,
    recv_task: JoinHandle<()>,
}

impl DtlsClientTransport {
    /// 连接到服务端并完成 DTLS 握手
    pub async fn connect(remote: &str, config: DtlsConfig) -> io::Result<Self> {
        let remote_addr: SocketAddr = remote.parse().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid remote addr `{}`: {}", remote, e),
            )
        })?;
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.connect(remote_addr).await?;
        Self::from_socket(socket, remote_addr, config).await
    }

    /// 使用预绑定且已 connect 的 [`UdpSocket`] 完成握手
    pub async fn from_socket(
        socket: UdpSocket,
        remote_addr: SocketAddr,
        config: DtlsConfig,
    ) -> io::Result<Self> {
        let local_addr = socket.local_addr()?;
        let socket_arc: Arc<dyn UtilConn + Send + Sync> = Arc::new(socket);

        let DtlsConfig {
            inner,
            handshake_timeout,
            overhead,
            send_queue_size,
            recv_buf_size,
        } = config;

        let conn = tokio::time::timeout(
            handshake_timeout,
            DTLSConn::new(socket_arc, inner, true, None),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "DTLS handshake timeout"))?
        .map_err(|e| io::Error::other(format!("DTLS handshake failed: {}", e)))?;

        let conn: Arc<dyn UtilConn + Send + Sync> = Arc::new(conn);
        let (send_tx, send_rx) = mpsc::channel::<Vec<u8>>(send_queue_size);
        let (decrypted_tx, decrypted_rx) = mpsc::channel::<Vec<u8>>(send_queue_size);
        let closed = Arc::new(AtomicBool::new(false));

        let send_task = spawn_writer_task(conn.clone(), send_rx, closed.clone(), remote_addr);
        let recv_task = spawn_reader_task(
            conn,
            decrypted_tx,
            closed.clone(),
            remote_addr,
            recv_buf_size,
        );

        Ok(Self {
            send_tx,
            decrypted_rx: TokioMutex::new(decrypted_rx),
            local_addr,
            remote_addr,
            overhead,
            closed,
            send_task,
            recv_task,
        })
    }

    pub fn remote_addr(&self) -> SocketAddr {
        self.remote_addr
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

impl Drop for DtlsClientTransport {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        self.send_task.abort();
        self.recv_task.abort();
    }
}

impl KcpTransport for DtlsClientTransport {
    fn try_send(&self, buf: &[u8]) -> io::Result<usize> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "DTLS conn closed"));
        }
        match self.send_tx.try_send(buf.to_vec()) {
            Ok(()) => Ok(buf.len()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "DTLS send queue full",
            )),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "DTLS conn closed",
            )),
        }
    }

    fn try_send_to(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize> {
        if target != self.remote_addr {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "DtlsClientTransport: target {} differs from remote {}",
                    target, self.remote_addr
                ),
            ));
        }
        self.try_send(buf)
    }

    fn recv<'a>(
        &'a self,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = io::Result<usize>> + Send + 'a>> {
        Box::pin(async move {
            let mut rx = self.decrypted_rx.lock().await;
            match rx.recv().await {
                Some(data) => {
                    let n = data.len().min(buf.len());
                    buf[..n].copy_from_slice(&data[..n]);
                    Ok(n)
                }
                None => Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "DTLS recv channel closed",
                )),
            }
        })
    }

    fn recv_from<'a>(
        &'a self,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = io::Result<(usize, SocketAddr)>> + Send + 'a>> {
        let remote = self.remote_addr;
        Box::pin(async move {
            let n = self.recv(buf).await?;
            Ok((n, remote))
        })
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local_addr)
    }

    fn overhead(&self) -> usize {
        self.overhead
    }
}

// ─── 服务端 ─────────────────────────────────────────────────────

struct DtlsSession {
    send_tx: mpsc::Sender<Vec<u8>>,
    send_task: JoinHandle<()>,
    recv_task: JoinHandle<()>,
}

impl Drop for DtlsSession {
    fn drop(&mut self) {
        self.send_task.abort();
        self.recv_task.abort();
    }
}

/// DTLS 服务端传输 — 多会话路由
///
/// 在指定 UDP 地址上接受多个 DTLS 客户端，按对端地址路由。
/// 每个客户端独立握手，独立加密通道。
pub struct DtlsServerTransport {
    sessions: Arc<DashMap<SocketAddr, Arc<DtlsSession>>>,
    decrypted_rx: TokioMutex<mpsc::Receiver<(Vec<u8>, SocketAddr)>>,
    local_addr: SocketAddr,
    overhead: usize,
    closed: Arc<AtomicBool>,
    accept_task: JoinHandle<()>,
}

impl DtlsServerTransport {
    /// 在指定地址绑定并启动 DTLS 监听器
    pub async fn bind(addr: &str, config: DtlsConfig) -> io::Result<Self> {
        let local: SocketAddr = addr.parse().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid bind addr `{}`: {}", addr, e),
            )
        })?;

        let DtlsConfig {
            inner,
            handshake_timeout,
            overhead,
            send_queue_size,
            recv_buf_size,
        } = config;

        let raw_listener = webrtc_dtls::listener::listen(local, inner)
            .await
            .map_err(|e| io::Error::other(format!("DTLS listen failed: {}", e)))?;

        let listener: Arc<dyn UtilListener + Send + Sync> = Arc::new(raw_listener);

        let local_addr = listener
            .addr()
            .await
            .map_err(|e| io::Error::other(format!("listener.addr failed: {}", e)))?;

        let sessions: Arc<DashMap<SocketAddr, Arc<DtlsSession>>> = Arc::new(DashMap::new());
        let (decrypted_tx, decrypted_rx) =
            mpsc::channel::<(Vec<u8>, SocketAddr)>(send_queue_size);
        let closed = Arc::new(AtomicBool::new(false));

        let accept_task = {
            let sessions = sessions.clone();
            let closed = closed.clone();
            let listener = listener;
            tokio::spawn(async move {
                run_accept_loop(
                    listener,
                    sessions,
                    decrypted_tx,
                    closed,
                    handshake_timeout,
                    send_queue_size,
                    recv_buf_size,
                )
                .await;
            })
        };

        Ok(Self {
            sessions,
            decrypted_rx: TokioMutex::new(decrypted_rx),
            local_addr,
            overhead,
            closed,
            accept_task,
        })
    }

    /// 当前活跃 DTLS 会话数
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// 主动断开指定对端的 DTLS 会话
    pub fn close_session(&self, peer: SocketAddr) -> bool {
        self.sessions.remove(&peer).is_some()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

impl Drop for DtlsServerTransport {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        self.accept_task.abort();
        self.sessions.clear();
    }
}

impl KcpTransport for DtlsServerTransport {
    fn try_send(&self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "DtlsServerTransport: use try_send_to with peer address",
        ))
    }

    fn try_send_to(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize> {
        let session = self
            .sessions
            .get(&target)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotConnected,
                    format!("DTLS session not found for peer {}", target),
                )
            })?
            .value()
            .clone();

        match session.send_tx.try_send(buf.to_vec()) {
            Ok(()) => Ok(buf.len()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "DTLS session send queue full",
            )),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.sessions.remove(&target);
                Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "DTLS session closed",
                ))
            }
        }
    }

    fn recv<'a>(
        &'a self,
        _buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = io::Result<usize>> + Send + 'a>> {
        Box::pin(async move {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "DtlsServerTransport: use recv_from",
            ))
        })
    }

    fn recv_from<'a>(
        &'a self,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = io::Result<(usize, SocketAddr)>> + Send + 'a>> {
        Box::pin(async move {
            let mut rx = self.decrypted_rx.lock().await;
            match rx.recv().await {
                Some((data, peer)) => {
                    let n = data.len().min(buf.len());
                    buf[..n].copy_from_slice(&data[..n]);
                    Ok((n, peer))
                }
                None => Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "DTLS server closed",
                )),
            }
        })
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local_addr)
    }

    fn overhead(&self) -> usize {
        self.overhead
    }
}

// ─── 内部任务 ─────────────────────────────────────────────────────

fn spawn_writer_task(
    conn: Arc<dyn UtilConn + Send + Sync>,
    mut send_rx: mpsc::Receiver<Vec<u8>>,
    closed: Arc<AtomicBool>,
    peer: SocketAddr,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(buf) = send_rx.recv().await {
            if closed.load(Ordering::SeqCst) {
                break;
            }
            if let Err(e) = conn.send(&buf).await {
                log::warn!("DTLS send error to {}: {}", peer, e);
                closed.store(true, Ordering::SeqCst);
                break;
            }
        }
    })
}

fn spawn_reader_task(
    conn: Arc<dyn UtilConn + Send + Sync>,
    decrypted_tx: mpsc::Sender<Vec<u8>>,
    closed: Arc<AtomicBool>,
    peer: SocketAddr,
    buf_size: usize,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buf = vec![0u8; buf_size];
        loop {
            if closed.load(Ordering::SeqCst) {
                break;
            }
            match conn.recv(&mut buf).await {
                Ok(n) => {
                    if decrypted_tx.send(buf[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    log::warn!("DTLS recv error from {}: {}", peer, e);
                    closed.store(true, Ordering::SeqCst);
                    break;
                }
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_accept_loop(
    listener: Arc<dyn UtilListener + Send + Sync>,
    sessions: Arc<DashMap<SocketAddr, Arc<DtlsSession>>>,
    decrypted_tx: mpsc::Sender<(Vec<u8>, SocketAddr)>,
    closed: Arc<AtomicBool>,
    handshake_timeout: Duration,
    send_queue_size: usize,
    recv_buf_size: usize,
) {
    while !closed.load(Ordering::SeqCst) {
        let accept_fut = listener.accept();
        let result = tokio::time::timeout(handshake_timeout, accept_fut).await;
        match result {
            Ok(Ok((conn, peer))) => {
                let (send_tx, send_rx) = mpsc::channel::<Vec<u8>>(send_queue_size);
                let session_closed = Arc::new(AtomicBool::new(false));

                let send_task = spawn_writer_task(conn.clone(), send_rx, session_closed.clone(), peer);

                let decrypted_tx_clone = decrypted_tx.clone();
                let sessions_for_cleanup = sessions.clone();
                let session_closed_recv = session_closed.clone();
                let recv_task = tokio::spawn(async move {
                    let mut buf = vec![0u8; recv_buf_size];
                    loop {
                        if session_closed_recv.load(Ordering::SeqCst) {
                            break;
                        }
                        match conn.recv(&mut buf).await {
                            Ok(n) => {
                                let pt = buf[..n].to_vec();
                                if decrypted_tx_clone.send((pt, peer)).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                log::warn!("DTLS server recv error from {}: {}", peer, e);
                                break;
                            }
                        }
                    }
                    session_closed_recv.store(true, Ordering::SeqCst);
                    sessions_for_cleanup.remove(&peer);
                });

                sessions.insert(
                    peer,
                    Arc::new(DtlsSession {
                        send_tx,
                        send_task,
                        recv_task,
                    }),
                );
                log::info!("DTLS handshake completed: peer={}", peer);
            }
            Ok(Err(e)) => {
                log::warn!("DTLS accept error: {}", e);
            }
            Err(_) => {
                // 单次 accept 超时（一般是恶意握手或对端掉线），跳过继续
                log::trace!("DTLS accept timeout, retrying");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dtls_config_psk_client() {
        let cfg = DtlsConfig::client_psk(b"secret".to_vec(), "kcp2");
        assert!(!cfg.inner.cipher_suites.is_empty());
        assert!(cfg.inner.psk.is_some());
        assert_eq!(cfg.overhead, DEFAULT_DTLS_OVERHEAD);
    }

    #[test]
    fn test_dtls_config_psk_server() {
        let cfg = DtlsConfig::server_psk(b"secret".to_vec(), "kcp2");
        assert!(cfg.inner.psk.is_some());
    }

    #[test]
    fn test_dtls_config_builder() {
        let cfg = DtlsConfig::client_psk(b"secret".to_vec(), "kcp2")
            .handshake_timeout(Duration::from_secs(3))
            .overhead(40)
            .send_queue_size(128)
            .recv_buf_size(2048);
        assert_eq!(cfg.handshake_timeout, Duration::from_secs(3));
        assert_eq!(cfg.overhead, 40);
        assert_eq!(cfg.send_queue_size, 128);
        assert_eq!(cfg.recv_buf_size, 2048);
    }

    fn find_free_addr() -> String {
        let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = sock.local_addr().unwrap();
        drop(sock);
        format!("127.0.0.1:{}", addr.port())
    }

    #[tokio::test]
    async fn test_dtls_handshake_psk() {
        let server_addr = find_free_addr();

        let server_cfg = DtlsConfig::server_psk(b"shared-secret".to_vec(), "kcp2")
            .handshake_timeout(Duration::from_secs(5));
        let server = DtlsServerTransport::bind(&server_addr, server_cfg)
            .await
            .expect("server bind");
        let server_addr_resolved = server.local_addr().unwrap();
        let server = Arc::new(server);

        let client_cfg = DtlsConfig::client_psk(b"shared-secret".to_vec(), "kcp2")
            .handshake_timeout(Duration::from_secs(5));
        let client = DtlsClientTransport::connect(&server_addr_resolved.to_string(), client_cfg)
            .await
            .expect("client handshake");

        // 客户端发送 → 服务端 recv_from
        let payload = b"hello dtls";
        client.try_send(payload).expect("client try_send");

        let mut buf = vec![0u8; 4096];
        let recv_result = tokio::time::timeout(
            Duration::from_secs(3),
            server.recv_from(&mut buf),
        )
        .await
        .expect("server recv_from timeout")
        .expect("server recv_from error");

        let (n, peer) = recv_result;
        assert_eq!(&buf[..n], payload);

        // 服务端反向发送 → 客户端 recv
        let pong = b"pong";
        server.try_send_to(pong, peer).expect("server try_send_to");
        let mut cbuf = vec![0u8; 4096];
        let n = tokio::time::timeout(Duration::from_secs(3), client.recv(&mut cbuf))
            .await
            .expect("client recv timeout")
            .expect("client recv error");
        assert_eq!(&cbuf[..n], pong);
    }
}

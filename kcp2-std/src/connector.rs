//! KCP 客户端连接器
//!
//! 提供灵活的 Builder 模式构建、稳定的任务生命周期管理。

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use std::mem::ManuallyDrop;

use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time;

use crate::buffer_pool::BufferPool;
use crate::config::KcpConfig;
use crate::connection::KcpConnection;
use crate::conv_generator::ConvGenerator;
use crate::transport::{KcpTransport, UdpTransport};

const DEFAULT_RECV_BUF_SIZE: usize = 65536;
const BUF_POOL_CAPACITY: usize = 64;

/// KCP 客户端连接器
///
/// 支持两种使用模式：
/// 1. **消费型 Builder**（原有 API）：`KcpConnector::new(addr).with_config(cfg).conv(1).connect().await`
/// 2. **非消费型配置**（新 API）：`connector.set_nodelay(...); connector.connect().await`
pub struct KcpConnector {
    remote_addr: SocketAddr,
    config: KcpConfig,
    conv: Option<u32>,
    recv_buf_size: usize,
    transport: Option<Arc<dyn KcpTransport>>,
    conv_generator: Option<ConvGenerator>,
}

/// KCP 会话，持有连接和所有后台任务句柄
///
/// 当 `KcpSession` 被 Drop 时，自动关闭连接并通知后台任务退出。
pub struct KcpSession {
    conn: Arc<KcpConnection>,
    recv_handle: AbortHandle,
    timeout_handle: AbortHandle,
    closed: Arc<AtomicBool>,
    shutdown_notify: Arc<Notify>,
}

use tokio::task::AbortHandle;

impl KcpSession {
    pub fn connection(&self) -> &Arc<KcpConnection> {
        &self.conn
    }

    pub async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.shutdown_notify.notify_waiters();
        self.conn.close();
    }

    pub async fn is_alive(&self) -> bool {
        !self.conn.is_dead().await
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

impl Drop for KcpSession {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        self.shutdown_notify.notify_waiters();
        self.recv_handle.abort();
        self.timeout_handle.abort();
        self.conn.close();
    }
}

impl KcpConnector {
    pub fn new(remote_addr: &str) -> io::Result<Self> {
        let remote_addr = remote_addr.parse().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid address: {}", e),
            )
        })?;

        Ok(Self {
            remote_addr,
            config: KcpConfig::default(),
            conv: None,
            recv_buf_size: DEFAULT_RECV_BUF_SIZE,
            transport: None,
            conv_generator: None,
        })
    }

    /// 使用预绑定的 UdpSocket 创建连接器（向后兼容）
    ///
    /// 允许外部控制 socket 绑定（如指定端口、设置 SO_REUSEADDR 等）。
    /// `connect()` 时会自动调用 `socket.connect(remote_addr)` 连接到远端。
    pub fn from_socket(socket: UdpSocket, remote_addr: &str, config: KcpConfig) -> io::Result<Self> {
        Self::from_transport(Arc::new(UdpTransport::new(socket)), remote_addr, config)
    }

    /// 使用自定义传输层创建连接器
    pub fn from_transport(
        transport: Arc<dyn KcpTransport>,
        remote_addr: &str,
        config: KcpConfig,
    ) -> io::Result<Self> {
        let remote_addr = remote_addr.parse().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid address: {}", e),
            )
        })?;

        Ok(Self {
            remote_addr,
            config,
            conv: None,
            recv_buf_size: DEFAULT_RECV_BUF_SIZE,
            transport: Some(transport),
            conv_generator: None,
        })
    }

    // ─── 消费型 Builder 方法（原有 API，保持向后兼容）───

    pub fn with_config(mut self, config: KcpConfig) -> Self {
        self.config = config;
        self
    }

    pub fn conv(mut self, conv: u32) -> Self {
        self.conv = Some(conv);
        self
    }

    pub fn nodelay(mut self, nodelay: bool, interval: u32, resend: u32, nc: bool) -> Self {
        self.config = self.config.nodelay(nodelay, interval, resend, nc);
        self
    }

    pub fn wndsize(mut self, sndwnd: u16, rcvwnd: u16) -> Self {
        self.config = self.config.wndsize(sndwnd, rcvwnd);
        self
    }

    pub fn mtu(mut self, mtu: usize) -> Self {
        self.config = self.config.mtu(mtu);
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.config = self.config.timeout(timeout);
        self
    }

    pub fn recv_buf_size(mut self, size: usize) -> Self {
        self.recv_buf_size = size.max(512);
        self
    }

    // ─── 非消费型配置方法（新 API，支持分步构建）───

    pub fn set_conv(&mut self, conv: u32) -> &mut Self {
        self.conv = Some(conv);
        self
    }

    pub fn set_nodelay(&mut self, nodelay: bool, interval: u32, resend: u32, nc: bool) -> &mut Self {
        self.config.nodelay = nodelay;
        self.config.interval = interval;
        self.config.resend = resend;
        self.config.nc = nc;
        self
    }

    pub fn set_wndsize(&mut self, sndwnd: u16, rcvwnd: u16) -> &mut Self {
        self.config.sndwnd = sndwnd;
        self.config.rcvwnd = rcvwnd;
        self
    }

    pub fn set_mtu(&mut self, mtu: usize) -> &mut Self {
        self.config.mtu = mtu;
        self
    }

    pub fn set_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.config.timeout = timeout;
        self
    }

    pub fn set_recv_buf_size(&mut self, size: usize) -> &mut Self {
        self.recv_buf_size = size.max(512);
        self
    }

    pub fn set_conv_generator(&mut self, gen: ConvGenerator) -> &mut Self {
        self.conv_generator = Some(gen);
        self
    }

    // ─── 连接方法 ───

    /// 建立 KCP 连接，返回 `KcpSession` 管理后台任务生命周期
    pub async fn connect(&self) -> io::Result<KcpSession> {
        let (session, _, _) = self.do_connect().await?;
        Ok(session)
    }

    /// 建立连接并额外返回 recv/timeout task 的 JoinHandle
    #[allow(clippy::type_complexity)]
    pub async fn connect_with_handles(&self) -> io::Result<(KcpSession, JoinHandle<()>, JoinHandle<()>)> {
        let (session, recv_task, timeout_task) = self.do_connect().await?;
        Ok((session, recv_task, timeout_task))
    }

    /// 旧版 API：建立连接并返回连接和 recv task 句柄
    ///
    /// 返回 `(Arc<KcpConnection>, JoinHandle<()>)` 保持向后兼容。
    /// Actor 自行管理 update 和 send，无需外部循环。
    /// 调用者负责在结束时 abort recv_task 并调用 conn.close()。
    pub async fn connect_with_recv_task(&self) -> io::Result<(Arc<KcpConnection>, JoinHandle<()>)> {
        let (session, recv_task, timeout_task) = self.do_connect().await?;
        let conn = session.connection().clone();
        timeout_task.abort();
        // ManuallyDrop 阻止 Drop impl 运行（不会 abort recv_task / close conn），
        // 但字段仍会正常 drop（Arc 减引用计数，AbortHandle 不 abort）
        let _ = ManuallyDrop::new(session);
        Ok((conn, recv_task))
    }

    async fn do_connect(&self) -> io::Result<(KcpSession, JoinHandle<()>, JoinHandle<()>)> {
        let (transport, recv_transport) = match &self.transport {
            Some(t) => {
                (t.clone(), t.clone())
            }
            None => {
                let s = UdpSocket::bind("0.0.0.0:0").await?;
                s.connect(self.remote_addr).await?;
                let t: Arc<dyn KcpTransport> = Arc::new(UdpTransport::new(s));
                (t.clone(), t)
            }
        };

        let conv = self.resolve_conv();
        let closed = Arc::new(AtomicBool::new(false));
        let shutdown_notify = Arc::new(Notify::new());

        let conn = Arc::new(KcpConnection::new(
            conv,
            self.remote_addr,
            &self.config,
            transport,
            true,
        ));

        let conn_clone = conn.clone();
        let recv_notify = shutdown_notify.clone();
        let buf_pool = Arc::new(BufferPool::new(BUF_POOL_CAPACITY, self.recv_buf_size));
        let recv_task = tokio::spawn(async move {
            let mut buf = buf_pool.get();
            loop {
                tokio::select! {
                    result = recv_transport.recv(&mut buf) => {
                        match result {
                            Ok(n) => {
                                if let Err(e) = conn_clone.input(&buf[..n]).await {
                                    log::error!("KCP input error: {}", e);
                                    break;
                                }
                            }
                            Err(e) => {
                                log::warn!("UDP recv error: {}", e);
                                break;
                            }
                        }
                    }
                    _ = recv_notify.notified() => {
                        break;
                    }
                }
            }
            buf_pool.put(buf);
        });

        let timeout_conn = conn.clone();
        let timeout_notify = shutdown_notify.clone();
        let timeout_duration = self.config.timeout;
        let timeout_task = tokio::spawn(async move {
            let check_interval = timeout_duration / 3;
            let mut ticker = time::interval(check_interval.max(Duration::from_millis(100)));
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let elapsed = kcp2_core::current()
                            .saturating_sub(timeout_conn.last_active_millis());
                        if elapsed > timeout_duration.as_millis().min(u32::MAX as u128) as u32 {
                            log::warn!(
                                "连接超时: conv={}, 最后活跃={}ms前, 超时={}ms",
                                timeout_conn.conv(),
                                elapsed,
                                timeout_duration.as_millis()
                            );
                            timeout_conn.close();
                            break;
                        }
                    }
                    _ = timeout_notify.notified() => {
                        break;
                    }
                }
            }
        });

        let session = KcpSession {
            conn,
            recv_handle: recv_task.abort_handle(),
            timeout_handle: timeout_task.abort_handle(),
            closed,
            shutdown_notify,
        };

        Ok((session, recv_task, timeout_task))
    }

    fn resolve_conv(&self) -> u32 {
        if let Some(conv) = self.conv {
            return conv;
        }
        if let Some(ref gen) = self.conv_generator {
            return gen.next();
        }
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_parses_valid_address() {
        assert!(KcpConnector::new("127.0.0.1:12345").is_ok());
    }

    #[test]
    fn test_new_rejects_invalid_address() {
        assert!(KcpConnector::new("not an address").is_err());
    }

    #[test]
    fn test_builder_chain() {
        let connector = KcpConnector::new("127.0.0.1:12345").unwrap()
            .nodelay(true, 10, 2, false)
            .wndsize(256, 256)
            .mtu(1400)
            .timeout(Duration::from_secs(30))
            .recv_buf_size(8192)
            .conv(42);

        assert_eq!(connector.conv, Some(42));
        assert_eq!(connector.recv_buf_size, 8192);
    }

    #[test]
    fn test_non_consuming_builder() {
        let mut connector = KcpConnector::new("127.0.0.1:12345").unwrap();
        connector.set_conv(99);
        connector.set_nodelay(true, 10, 2, false);
        connector.set_wndsize(512, 512);
        connector.set_mtu(1200);
        connector.set_recv_buf_size(4096);
        connector.set_timeout(Duration::from_secs(60));

        assert_eq!(connector.conv, Some(99));
        assert_eq!(connector.recv_buf_size, 4096);
        assert_eq!(connector.config.timeout, Duration::from_secs(60));
    }

    #[test]
    fn test_recv_buf_size_minimum() {
        let connector = KcpConnector::new("127.0.0.1:12345").unwrap()
            .recv_buf_size(100);
        assert_eq!(connector.recv_buf_size, 512);
    }

    #[test]
    fn test_resolve_conv_manual() {
        let connector = KcpConnector::new("127.0.0.1:12345").unwrap()
            .conv(42);
        assert_eq!(connector.resolve_conv(), 42);
    }

    #[test]
    fn test_resolve_conv_default() {
        let connector = KcpConnector::new("127.0.0.1:12345").unwrap();
        assert_eq!(connector.resolve_conv(), 1);
    }

    #[test]
    fn test_resolve_conv_generator() {
        let mut connector = KcpConnector::new("127.0.0.1:12345").unwrap();
        connector.set_conv_generator(ConvGenerator::new(100));
        assert_eq!(connector.resolve_conv(), 100);
        assert_eq!(connector.resolve_conv(), 101);
    }

    #[test]
    fn test_resolve_conv_manual_overrides_generator() {
        let mut connector = KcpConnector::new("127.0.0.1:12345").unwrap()
            .conv(42);
        connector.set_conv_generator(ConvGenerator::new(100));
        assert_eq!(connector.resolve_conv(), 42);
    }

    fn find_free_addr() -> String {
        let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = sock.local_addr().unwrap();
        drop(sock);
        format!("127.0.0.1:{}", addr.port())
    }

    #[tokio::test]
    async fn test_connect_basic() {
        let addr = find_free_addr();
        let connector = KcpConnector::new(&addr).unwrap()
            .conv(1);

        let result = connector.connect().await;
        assert!(result.is_ok(), "connect should succeed: {:?}", result.err());

        let session = result.unwrap();
        assert!(session.is_alive().await);
    }

    #[tokio::test]
    async fn test_connect_with_recv_task() {
        let addr = find_free_addr();
        let connector = KcpConnector::new(&addr).unwrap()
            .conv(1);

        let result = connector.connect_with_recv_task().await;
        assert!(result.is_ok(), "connect_with_recv_task should succeed");

        let (conn, _handle) = result.unwrap();
        assert!(!conn.is_dead().await);
    }

    #[tokio::test]
    async fn test_session_close() {
        let addr = find_free_addr();
        let connector = KcpConnector::new(&addr).unwrap()
            .conv(1);

        let session = connector.connect().await.unwrap();
        assert!(session.is_alive().await);

        session.close().await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if !session.is_alive().await && session.is_closed() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("session should be closed within 2s");
        assert!(!session.is_alive().await);
        assert!(session.is_closed());
    }

    #[tokio::test]
    async fn test_from_socket_connects() {
        let socket = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let addr = find_free_addr();
        let config = KcpConfig::default().nodelay(true, 10, 2, false);

        let connector = KcpConnector::from_socket(socket, &addr, config).unwrap();
        assert_eq!(connector.remote_addr, addr.parse::<SocketAddr>().unwrap());

        let result = connector.conv(1).connect().await;
        assert!(result.is_ok(), "from_socket connect should succeed: {:?}", result.err());
    }

    #[tokio::test]
    async fn test_connect_with_handles() {
        let addr = find_free_addr();
        let connector = KcpConnector::new(&addr).unwrap().conv(1);
        let result = connector.connect_with_handles().await;
        assert!(result.is_ok(), "connect_with_handles should succeed: {:?}", result.err());

        let (session, recv_handle, timeout_handle) = result.unwrap();
        assert!(!recv_handle.is_finished(), "recv task should be running");
        assert!(!timeout_handle.is_finished(), "timeout task should be running");
        assert!(session.is_alive().await);

        session.close().await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if session.is_closed() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("session should be closed within 2s");
        assert!(session.is_closed());
    }

    #[tokio::test]
    async fn test_from_transport() {
        use crate::transport::{KcpTransport, UdpTransport};

        let addr = find_free_addr();
        let socket = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let transport: Arc<dyn KcpTransport> = Arc::new(UdpTransport::new(socket));
        let config = KcpConfig::default().nodelay(true, 10, 2, false);

        let connector = KcpConnector::from_transport(transport, &addr, config).unwrap();
        assert_eq!(connector.remote_addr.to_string(), addr);

        let result = connector.conv(1).connect().await;
        assert!(result.is_ok(), "from_transport connect should succeed: {:?}", result.err());
    }
}

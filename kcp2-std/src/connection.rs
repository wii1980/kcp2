use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;

use crate::async_kcp::{ActorConfig, AsyncKcp};
use crate::transport::KcpTransport;
use kcp2_core::{current, Result};
use crate::config::KcpConfig;

type KcpOutputFn = Box<dyn Fn(&[u8]) + Send + Sync>;

pub struct KcpConnection {
    kcp: Arc<AsyncKcp<KcpOutputFn>>,
    conv: u32,
    addr: SocketAddr,
    last_active: AtomicU64,
}

impl KcpConnection {
    pub(crate) fn new(
        conv: u32,
        addr: SocketAddr,
        config: &KcpConfig,
        transport: Arc<dyn KcpTransport>,
        connected: bool,
    ) -> Self {
        let mut actor_config = ActorConfig::from_kcp_config(config);
        actor_config.conv = conv;
        // 在 crypto overhead 之外，再扣除 transport overhead（如 DTLS Record + AEAD）
        actor_config.mtu = actor_config.mtu.saturating_sub(transport.overhead());
        let kcp = Arc::new(AsyncKcp::new_with_transport(
            &actor_config,
            transport,
            addr,
            connected,
        ));

        let last_active = AtomicU64::new(current() as u64);

        Self {
            kcp,
            conv,
            addr,
            last_active,
        }
    }

    pub fn conv(&self) -> u32 {
        self.conv
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn update_last_active(&self) {
        self.last_active.store(current() as u64, Ordering::Release);
    }

    pub fn last_active_millis(&self) -> u32 {
        self.last_active.load(Ordering::Acquire) as u32
    }

    pub async fn send(&self, data: &[u8]) -> Result<usize> {
        self.update_last_active();
        self.kcp.send(data).await
    }

    pub async fn recv(&self, buf: &mut [u8]) -> Result<usize> {
        self.update_last_active();
        self.kcp.recv(buf).await
    }

    pub async fn try_recv(&self, buf: &mut [u8]) -> Result<usize> {
        self.kcp.try_recv(buf).await
    }

    pub async fn input(&self, data: &[u8]) -> Result<usize> {
        self.update_last_active();
        self.kcp.input(data).await
    }

    pub async fn input_bytes(&self, data: Bytes) -> Result<usize> {
        self.update_last_active();
        self.kcp.input_bytes(data).await
    }

    pub async fn is_dead(&self) -> bool {
        self.kcp.is_dead().await
    }

    pub async fn wait_snd(&self) -> usize {
        self.kcp.wait_snd().await
    }

    pub async fn send_and_wait_ack(&self, data: &[u8]) -> Result<()> {
        self.update_last_active();
        self.kcp.send_and_wait_ack(data).await
    }

    pub async fn send_and_wait_ack_with_timeout(
        &self,
        data: &[u8],
        timeout: Duration,
    ) -> Result<()> {
        self.update_last_active();
        self.kcp.send_and_wait_ack_with_timeout(data, timeout).await
    }

    pub async fn wait_all_sent(&self) -> Result<()> {
        self.kcp.wait_all_sent().await
    }

    pub fn kcp(&self) -> &Arc<AsyncKcp<KcpOutputFn>> {
        &self.kcp
    }

    /// 关闭连接，标记为死亡状态
    /// 调用后 send/recv 将返回 DeadLink 错误
    pub fn close(&self) {
        self.kcp.kill();
    }
}

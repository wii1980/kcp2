//! 异步 KCP 封装 — Actor 模式
//!
//! 架构：
//! - `kcp_cmd`：命令枚举和挂起请求类型
//! - `kcp_actor`：KcpActor — 单 tokio task 独占 Kcp 实例，无锁，直接 socket 发送
//! - `kcp_handle`：KcpHandle — 持有 `mpsc::Sender<KcpCmd>`，通过 channel 发命令
//! - `kcp_callback_actor`：无 socket 模式，output 通过回调发送（测试兼容）
//! - `AsyncKcp`：兼容层，内部持有 KcpHandle，保持旧公共 API

use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

use kcp2_core::{Kcp, KcpOutput, SendHandle, Result};

use crate::crypto::KcpCrypto;
use crate::transport::{KcpTransport, UdpTransport};

mod actor;
mod callback_actor;
mod cmd;
mod handle;

use self::actor::KcpActor;
use self::callback_actor::run_callback_actor;
pub(crate) use self::handle::KcpHandle;

// ─── AsyncKcp（兼容层）──────────────────────────────────────

/// 异步 KCP 兼容层
///
/// 保持旧公共 API 不变，内部委托给 KcpHandle。
pub struct AsyncKcp<Output: KcpOutput + Send + 'static> {
    handle: KcpHandle,
    _shutdown_tx: watch::Sender<bool>,
    _phantom: PhantomData<Output>,
}

impl<Output: KcpOutput + Send + 'static> Unpin for AsyncKcp<Output> {}

pub(crate) struct ActorConfig {
    pub conv: u32,
    pub nodelay: bool,
    pub interval: u32,
    pub resend: u32,
    pub nc: bool,
    pub sndwnd: u16,
    pub rcvwnd: u16,
    pub mtu: usize,
    pub rx_minrto: u32,
    pub dead_link: u32,
    pub stream: bool,
    pub crypto: Option<Arc<dyn KcpCrypto>>,
}

impl ActorConfig {
    pub fn from_kcp_config(config: &crate::config::KcpConfig) -> Self {
        Self {
            conv: 0, // 由调用者设置
            nodelay: config.nodelay,
            interval: config.interval,
            resend: config.resend,
            nc: config.nc,
            sndwnd: config.sndwnd,
            rcvwnd: config.rcvwnd,
            mtu: config.effective_mtu(),
            rx_minrto: config.rx_minrto,
            dead_link: config.dead_link,
            stream: config.stream,
            crypto: config.crypto.clone(),
        }
    }
}

impl<Output: KcpOutput + Send + 'static> AsyncKcp<Output> {
    /// 创建新的异步 KCP（兼容旧 API）
    ///
    /// 内部启动 Actor task，output 回调在 Actor 内通过收集器间接调用。
    pub fn new(conv: u32, output: Output) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        // 为无 socket 模式创建 output callback wrapper
        let output = Arc::new(std::sync::Mutex::new(Some(output)));

        // 启动 Actor（无 socket 模式，output 通过 callback 调用）
        let output_clone = output.clone();
        tokio::spawn(async move {
            run_callback_actor(conv, cmd_rx, shutdown_rx, output_clone).await;
        });

        Self {
            handle: KcpHandle::new(cmd_tx),
            _shutdown_tx: shutdown_tx,
            _phantom: PhantomData,
        }
    }

    /// 创建绑定到 transport 的 AsyncKcp（生产用）
    pub(crate) fn new_with_transport(
        actor_config: &ActorConfig,
        transport: Arc<dyn KcpTransport>,
        peer: SocketAddr,
        connected: bool,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let mut actor = KcpActor::new(
            actor_config.conv,
            transport,
            peer,
            connected,
            cmd_rx,
            shutdown_rx,
            actor_config.crypto.clone(),
        );

        // 应用配置
        actor.kcp_mut().set_nodelay(
            actor_config.nodelay,
            actor_config.interval,
            actor_config.resend,
            actor_config.nc,
        );
        actor
            .kcp_mut()
            .set_wndsize(actor_config.sndwnd, actor_config.rcvwnd);
        let _ = actor.kcp_mut().set_mtu(actor_config.mtu);
        actor.kcp_mut().set_rx_minrto(actor_config.rx_minrto);
        actor.kcp_mut().set_dead_link(actor_config.dead_link);
        actor.kcp_mut().set_stream(actor_config.stream);

        tokio::spawn(async move {
            actor.run().await;
        });

        Self {
            handle: KcpHandle::new(cmd_tx),
            _shutdown_tx: shutdown_tx,
            _phantom: PhantomData,
        }
    }

    /// 创建绑定到 socket 的 AsyncKcp（向后兼容）
    ///
    /// 内部将 UdpSocket 包装为 UdpTransport。
    #[allow(dead_code)]
    pub(crate) fn new_with_socket(
        actor_config: &ActorConfig,
        socket: Arc<UdpSocket>,
        peer: SocketAddr,
        connected: bool,
    ) -> Self {
        let transport = Arc::new(UdpTransport::from_arc(socket));
        Self::new_with_transport(actor_config, transport as Arc<dyn KcpTransport>, peer, connected)
    }

    // ─── 公共 API（保持不变）────────────────────────────

    pub fn set_nodelay(&self, _nodelay: bool, _interval: u32, _resend: u32, _nc: bool) {
        log::warn!("set_nodelay called after construction, this is a no-op in Actor mode");
    }

    pub fn set_wndsize(&self, _sndwnd: u16, _rcvwnd: u16) {
        log::warn!("set_wndsize called after construction, this is a no-op in Actor mode");
    }

    pub fn set_mtu(&self, _mtu: usize) -> Result<()> {
        log::warn!("set_mtu called after construction, this is a no-op in Actor mode");
        Ok(())
    }

    pub fn set_interval(&self, _interval: u32) {
        log::warn!("set_interval called after construction, this is a no-op in Actor mode");
    }

    pub fn set_stream(&self, _stream: bool) {
        log::warn!("set_stream called after construction, this is a no-op in Actor mode");
    }

    pub fn set_rx_minrto(&self, _minrto: u32) {
        log::warn!("set_rx_minrto called after construction, this is a no-op in Actor mode");
    }

    pub fn set_dead_link(&self, _dead_link: u32) {
        log::warn!("set_dead_link called after construction, this is a no-op in Actor mode");
    }

    pub fn set_maximum_resend_times(&self, _times: u32) {
        log::warn!(
            "set_maximum_resend_times called after construction, this is a no-op in Actor mode"
        );
    }

    pub async fn input(&self, data: &[u8]) -> Result<usize> {
        self.handle.input(data).await
    }

    pub async fn input_bytes(&self, data: Bytes) -> Result<usize> {
        self.handle.input_bytes(data).await
    }

    pub async fn send(&self, data: &[u8]) -> Result<usize> {
        self.handle.send(data).await
    }

    pub async fn send_with_handle(&self, data: &[u8]) -> Result<SendHandle> {
        self.handle.send_with_handle(data).await
    }

    pub async fn is_send_acked(&self, handle: SendHandle) -> bool {
        self.handle.is_send_acked(handle).await
    }

    pub async fn send_and_wait_ack(&self, data: &[u8]) -> Result<()> {
        self.handle.send_and_wait_ack(data).await
    }

    pub async fn wait_ack(&self, handle: SendHandle) -> Result<()> {
        self.handle.wait_ack(handle).await
    }

    pub async fn wait_ack_with_timeout(
        &self,
        handle: SendHandle,
        timeout: Duration,
    ) -> Result<()> {
        self.handle.wait_ack_with_timeout(handle, timeout).await
    }

    pub async fn send_and_wait_ack_with_timeout(
        &self,
        data: &[u8],
        timeout: Duration,
    ) -> Result<()> {
        self.handle
            .send_and_wait_ack_with_timeout(data, timeout)
            .await
    }

    pub async fn wait_all_sent(&self) -> Result<()> {
        self.handle.wait_all_sent().await
    }

    pub async fn is_dead(&self) -> bool {
        self.handle.is_dead().await
    }

    pub async fn try_recv(&self, buf: &mut [u8]) -> Result<usize> {
        let data = self.handle.try_recv().await?;
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }

    pub async fn recv(&self, buf: &mut [u8]) -> Result<usize> {
        let data = self.handle.recv().await?;
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }

    pub async fn update(&self, _current: u32) {
        // Actor 内部自动 update，此方法为空操作
    }

    pub async fn flush(&self) {
        // Actor 内部在每次操作后自动 flush，此方法为空操作
    }

    pub async fn wait_snd(&self) -> usize {
        self.handle.wait_snd().await
    }

    pub async fn reset_rto(&self) {
        self.handle.reset_rto().await
    }

    pub fn kill(&self) {
        self.handle.kill();
    }

    pub fn notify(&self) -> Arc<tokio::sync::Notify> {
        // 兼容旧 API，返回一个空 Notify（不再使用）
        Arc::new(tokio::sync::Notify::new())
    }

    pub async fn send_reconnect(&self) -> Result<()> {
        self.handle.send_reconnect().await
    }

    pub fn inner(&self) -> Arc<parking_lot::RwLock<Kcp<Box<dyn KcpOutput + Send + Sync>>>> {
        // 兼容旧 API — 不再返回真实的内部 Kcp
        log::warn!("inner() is deprecated in Actor mode, returns empty Kcp");
        Arc::new(parking_lot::RwLock::new(Kcp::new(0, Box::new(|_| {}))))
    }
}

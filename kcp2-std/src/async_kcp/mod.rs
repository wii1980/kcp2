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
use tokio::sync::{mpsc, watch};

use kcp2_core::{KcpError, KcpOutput, Result};

use crate::crypto::KcpCrypto;
use crate::transport::KcpTransport;

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
    pub channel_capacity: usize,
    pub pending_send_cap: usize,
    pub recv_timeout_ms: u64,
    pub output_queue_size: usize,
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
            channel_capacity: config.channel_capacity,
            pending_send_cap: config.pending_send_cap,
            recv_timeout_ms: config.timeout.as_millis() as u64,
            output_queue_size: config.output_queue_size,
        }
    }
}

impl<Output: KcpOutput + Send + 'static> AsyncKcp<Output> {
    /// 创建新的异步 KCP（兼容旧 API）
    ///
    /// 内部启动 Actor task，output 回调在 Actor 内通过收集器间接调用。
    pub fn new(conv: u32, output: Output) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(16);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        // 为无 socket 模式创建 output callback wrapper
        let output = Arc::new(std::sync::Mutex::new(Some(output)));

        // 启动 Actor（无 socket 模式，output 通过 callback 调用）
        let output_clone = output.clone();
        tokio::spawn(async move {
            run_callback_actor(conv, cmd_rx, shutdown_rx, output_clone).await;
        });

        Self {
            handle: KcpHandle::new(cmd_tx, shutdown_tx.clone()),
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
        let (cmd_tx, cmd_rx) = mpsc::channel(actor_config.channel_capacity);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let mut actor = KcpActor::new(
            actor_config.conv,
            transport,
            peer,
            connected,
            cmd_rx,
            shutdown_rx,
            actor_config.crypto.clone(),
            actor_config.pending_send_cap,
            actor_config.recv_timeout_ms,
            actor_config.output_queue_size,
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
        if let Err(e) = actor.kcp_mut().set_mtu(actor_config.mtu) {
            log::error!("Failed to set MTU to {}: {e}", actor_config.mtu);
        }
        actor.kcp_mut().set_rx_minrto(actor_config.rx_minrto);
        actor.kcp_mut().set_dead_link(actor_config.dead_link);
        actor.kcp_mut().set_stream(actor_config.stream);

        tokio::spawn(async move {
            actor.run().await;
        });

        Self {
            handle: KcpHandle::new(cmd_tx, shutdown_tx.clone()),
            _shutdown_tx: shutdown_tx,
            _phantom: PhantomData,
        }
    }

    // ─── 公共 API（保持不变）────────────────────────────

    pub async fn input(&self, data: &[u8]) -> Result<usize> {
        self.handle.input(data).await
    }

    pub async fn input_bytes(&self, data: Bytes) -> Result<usize> {
        self.handle.input_bytes(data).await
    }

    pub async fn send(&self, data: &[u8]) -> Result<usize> {
        self.handle.send(data).await
    }

    pub async fn send_and_wait_ack(&self, data: &[u8]) -> Result<()> {
        self.handle.send_and_wait_ack(data).await
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
        if data.len() > buf.len() {
            return Err(KcpError::BufferTooSmall {
                required: data.len(),
                available: buf.len(),
            });
        }
        buf[..data.len()].copy_from_slice(&data);
        Ok(data.len())
    }

    pub async fn recv(&self, buf: &mut [u8]) -> Result<usize> {
        let data = self.handle.recv().await?;
        if data.len() > buf.len() {
            return Err(KcpError::BufferTooSmall {
                required: data.len(),
                available: buf.len(),
            });
        }
        buf[..data.len()].copy_from_slice(&data);
        Ok(data.len())
    }

    pub async fn wait_snd(&self) -> usize {
        self.handle.wait_snd().await
    }

    pub fn kill(&self) {
        self.handle.kill();
    }

    pub async fn send_reconnect(&self) -> Result<()> {
        self.handle.send_reconnect().await
    }

}

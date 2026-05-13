//! KCP 句柄 — 通过 channel 与 Actor 通信

use bytes::{Bytes, BytesMut};
use std::time::Duration;

use kcp2_core::{KcpError, SendHandle, Result};
use tokio::sync::{mpsc, oneshot};

use super::cmd::KcpCmd;

/// KCP 句柄 — 通过 channel 与 Actor 通信
///
/// Clone 安全，可跨 task 共享。
#[derive(Clone)]
pub(crate) struct KcpHandle {
    pub(crate) cmd_tx: mpsc::Sender<KcpCmd>,
}

impl KcpHandle {
    pub(crate) fn new(cmd_tx: mpsc::Sender<KcpCmd>) -> Self {
        Self { cmd_tx }
    }

    /// 发送数据
    pub(crate) async fn send(&self, data: &[u8]) -> Result<usize> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(KcpCmd::Send {
                data: Bytes::copy_from_slice(data),
                ack: tx,
            })
            .await
            .map_err(|_| KcpError::DeadLink)?;
        rx.await.map_err(|_| KcpError::DeadLink)?
    }

    /// 批量发送数据（一次 channel 通信，减少往返开销）
    #[allow(dead_code)]
    pub(crate) async fn send_batch(&self, data: Vec<&[u8]>) -> Result<usize> {
        let (tx, rx) = oneshot::channel();
        let bytes_data: Vec<Bytes> = data.iter().map(|d| Bytes::copy_from_slice(d)).collect();
        self.cmd_tx
            .send(KcpCmd::SendBatch {
                data: bytes_data,
                ack: tx,
            })
            .await
            .map_err(|_| KcpError::DeadLink)?;
        rx.await.map_err(|_| KcpError::DeadLink)?
    }

    /// 输入数据（从网络收到的数据），fire-and-forget
    pub(crate) async fn input(&self, data: &[u8]) -> Result<usize> {
        self.cmd_tx
            .send(KcpCmd::Input {
                data: Bytes::copy_from_slice(data),
            })
            .await
            .map_err(|_| KcpError::DeadLink)?;
        Ok(0)
    }

    /// 输入 Bytes 数据，fire-and-forget
    pub(crate) async fn input_bytes(&self, data: Bytes) -> Result<usize> {
        self.cmd_tx
            .send(KcpCmd::Input { data })
            .await
            .map_err(|_| KcpError::DeadLink)?;
        Ok(0)
    }

    /// 异步接收（等待数据）
    pub(crate) async fn recv(&self) -> Result<BytesMut> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(KcpCmd::Recv { ack: tx })
            .await
            .map_err(|_| KcpError::DeadLink)?;
        rx.await.map_err(|_| KcpError::DeadLink)?
    }

    /// 同步接收（非阻塞）
    pub(crate) async fn try_recv(&self) -> Result<BytesMut> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(KcpCmd::TryRecv { ack: tx })
            .await
            .map_err(|_| KcpError::DeadLink)?;
        rx.await.map_err(|_| KcpError::DeadLink)?
    }

    /// 发送数据并等待确认
    pub(crate) async fn send_and_wait_ack(&self, data: &[u8]) -> Result<()> {
        let handle = self.send_with_handle(data).await?;
        self.wait_ack(handle).await
    }

    /// 发送数据并返回句柄
    pub(crate) async fn send_with_handle(&self, data: &[u8]) -> Result<SendHandle> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(KcpCmd::SendWithHandle {
                data: Bytes::copy_from_slice(data),
                ack: tx,
            })
            .await
            .map_err(|_| KcpError::DeadLink)?;
        rx.await.map_err(|_| KcpError::DeadLink)?
    }

    /// 发送数据并等待确认（带超时）
    pub(crate) async fn send_and_wait_ack_with_timeout(
        &self,
        data: &[u8],
        timeout: Duration,
    ) -> Result<()> {
        let handle = self.send_with_handle(data).await?;
        self.wait_ack_with_timeout(handle, timeout).await
    }

    /// 等待 ACK
    pub(crate) async fn wait_ack(&self, handle: SendHandle) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(KcpCmd::WaitAck { handle, ack: tx })
            .await
            .map_err(|_| KcpError::DeadLink)?;
        rx.await.map_err(|_| KcpError::DeadLink)?
    }

    /// 等待 ACK（带超时）
    pub(crate) async fn wait_ack_with_timeout(
        &self,
        handle: SendHandle,
        timeout: Duration,
    ) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(KcpCmd::WaitAckTimeout {
                handle,
                timeout,
                ack: tx,
            })
            .await
            .map_err(|_| KcpError::DeadLink)?;
        rx.await.map_err(|_| KcpError::DeadLink)?
    }

    /// 等待所有发送数据被确认
    pub(crate) async fn wait_all_sent(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(KcpCmd::WaitAllSent { ack: tx })
            .await
            .map_err(|_| KcpError::DeadLink)?;
        rx.await.map_err(|_| KcpError::DeadLink)?
    }

    /// 检查连接是否已死
    pub(crate) async fn is_dead(&self) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(KcpCmd::IsDead { ack: tx })
            .await
            .is_err()
        {
            return true;
        }
        rx.await.unwrap_or(true)
    }

    /// 获取等待发送的数据量
    pub(crate) async fn wait_snd(&self) -> usize {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(KcpCmd::WaitSnd { ack: tx })
            .await
            .is_err()
        {
            return 0;
        }
        rx.await.unwrap_or(0)
    }

    /// 检查句柄是否已确认
    pub(crate) async fn is_send_acked(&self, handle: SendHandle) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(KcpCmd::IsSendAcked { handle, ack: tx })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// 强制标记连接为死亡状态
    pub(crate) fn kill(&self) {
        // kill 是 fire-and-forget，用 try_send 避免在 sync 上下文中 await
        if self.cmd_tx.try_send(KcpCmd::Kill).is_err() {
            log::warn!("KcpHandle::kill: command channel full, kill command dropped");
        }
    }

        /// 发送 CMD_RECONNECT
    pub(crate) async fn send_reconnect(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(KcpCmd::SendReconnect { ack: tx })
            .await
            .map_err(|_| KcpError::DeadLink)?;
        rx.await.map_err(|_| KcpError::DeadLink)?
    }

    pub(crate) async fn reset_rto(&self) {
        let _ = self.cmd_tx.send(KcpCmd::ResetRto).await;
    }
}

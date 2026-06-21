//! KCP 句柄 — 通过 channel 与 Actor 通信

use bytes::{Bytes, BytesMut};
use std::time::Duration;

use kcp2_core::{KcpError, SendHandle, Result};
use tokio::sync::{mpsc, oneshot, watch};

use super::cmd::KcpCmd;

/// KCP 句柄 — 通过 channel 与 Actor 通信
///
/// Clone 安全，可跨 task 共享。
#[derive(Clone)]
pub(crate) struct KcpHandle {
    pub(crate) cmd_tx: mpsc::Sender<KcpCmd>,
    shutdown_tx: watch::Sender<bool>,
}

impl KcpHandle {
    pub(crate) fn new(
        cmd_tx: mpsc::Sender<KcpCmd>,
        shutdown_tx: watch::Sender<bool>,
    ) -> Self {
        Self {
            cmd_tx,
            shutdown_tx,
        }
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

    /// 输入数据（从网络收到的数据），fire-and-forget
    pub(crate) async fn input(&self, data: &[u8]) -> Result<usize> {
        let len = data.len();
        self.cmd_tx
            .send(KcpCmd::Input {
                data: Bytes::copy_from_slice(data),
            })
            .await
            .map_err(|_| KcpError::DeadLink)?;
        Ok(len)
    }

    /// 输入 Bytes 数据，fire-and-forget
    pub(crate) async fn input_bytes(&self, data: Bytes) -> Result<usize> {
        let len = data.len();
        self.cmd_tx
            .send(KcpCmd::Input { data })
            .await
            .map_err(|_| KcpError::DeadLink)?;
        Ok(len)
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
    /// 强制标记连接为死亡状态
    pub(crate) fn kill(&self) {
        // Use the shutdown watch channel for guaranteed delivery
        // (watch channel is unbounded, unlike the command mpsc channel)
        let _ = self.shutdown_tx.send(true);
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
}

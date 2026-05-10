//! KCP Actor 命令枚举

use bytes::{Bytes, BytesMut};
use std::time::Duration;

use kcp2_core::{SendHandle, Result};
use tokio::sync::oneshot;

/// Actor 命令枚举
pub(crate) enum KcpCmd {
    // 数据面
    Send {
        data: Bytes,
        ack: oneshot::Sender<Result<usize>>,
    },
    #[allow(dead_code)]
    SendBatch {
        data: Vec<Bytes>,
        ack: oneshot::Sender<Result<usize>>,
    },
    Input {
        data: Bytes,
    },
    Recv {
        ack: oneshot::Sender<Result<BytesMut>>,
    },
    TryRecv {
        ack: oneshot::Sender<Result<BytesMut>>,
    },
    SendWithHandle {
        data: Bytes,
        ack: oneshot::Sender<Result<SendHandle>>,
    },
    WaitAck {
        handle: SendHandle,
        ack: oneshot::Sender<Result<()>>,
    },
    WaitAckTimeout {
        handle: SendHandle,
        timeout: Duration,
        ack: oneshot::Sender<Result<()>>,
    },
    WaitAllSent {
        ack: oneshot::Sender<Result<()>>,
    },

    // 控制面
    IsDead {
        ack: oneshot::Sender<bool>,
    },
    WaitSnd {
        ack: oneshot::Sender<usize>,
    },
    IsSendAcked {
        handle: SendHandle,
        ack: oneshot::Sender<bool>,
    },
    Kill,
    SendReconnect {
        ack: oneshot::Sender<Result<()>>,
    },
    ResetRto,
}

/// 挂起的 wait_ack 请求
pub(crate) struct PendingWaitAck {
    pub handle: SendHandle,
    pub deadline: Option<tokio::time::Instant>,
    pub ack: oneshot::Sender<Result<()>>,
}

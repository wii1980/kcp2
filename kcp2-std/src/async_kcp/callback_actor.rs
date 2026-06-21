//! 无 socket 的 Callback Actor — output 通过回调发送（用于测试兼容）

use bytes::BytesMut;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};

use kcp2_core::{Kcp, KcpOutput, current, KcpError, Result};

use super::cmd::{KcpCmd, PendingWaitAck};

/// 无 socket 的 Actor 变体，output 通过回调发送
pub(super) async fn run_callback_actor<Output: KcpOutput + Send + 'static>(
    conv: u32,
    mut cmd_rx: mpsc::Receiver<KcpCmd>,
    mut shutdown_rx: watch::Receiver<bool>,
    output: Arc<std::sync::Mutex<Option<Output>>>,
) {
    // 创建带 callback 的 output collector
    let collected: Arc<std::sync::Mutex<Vec<Vec<u8>>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut kcp = {
        let c = collected.clone();
        let output_fn = Box::new(move |data: &[u8]| {
            c.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(data.to_vec());
        }) as Box<dyn Fn(&[u8]) + Send + Sync>;
        let mut kcp = Kcp::new(conv, output_fn);
        kcp.update(0);
        kcp
    };

    let mut pending_recv: Option<oneshot::Sender<Result<BytesMut>>> = None;
    let mut pending_wait_acks: Vec<PendingWaitAck> = Vec::new();
    let mut pending_wait_all: Vec<oneshot::Sender<Result<()>>> = Vec::new();
    let mut recv_tmp = BytesMut::zeroed(65536);

    let mut next_update = tokio::time::Instant::now();

    loop {
        tokio::select! {
            biased;

            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(c) => {
                        handle_callback_cmd_deferred(
                            &mut kcp,
                            c,
                            &mut pending_recv,
                            &mut pending_wait_acks,
                            &mut pending_wait_all,
                            &mut recv_tmp,
                        );
                        while let Ok(c) = cmd_rx.try_recv() {
                            handle_callback_cmd_deferred(
                                &mut kcp,
                                c,
                                &mut pending_recv,
                                &mut pending_wait_acks,
                                &mut pending_wait_all,
                                &mut recv_tmp,
                            );
                        }
                        callback_post_batch(
                            &mut kcp,
                            &collected,
                            &output,
                            &mut pending_recv,
                            &mut pending_wait_acks,
                            &mut pending_wait_all,
                            &mut recv_tmp,
                        );
                    }
                    None => break,
                }
                let now_ms = current();
                let next_ms = kcp.check(now_ms);
                let delay_ms = next_ms.saturating_sub(now_ms);
                next_update = tokio::time::Instant::now() + Duration::from_millis(delay_ms as u64);
            }

            _ = tokio::time::sleep_until(next_update), if kcp.needs_update() => {
                kcp.update(current());
                drain_callback_output(&collected, &output);
                try_wake_recv_inner(&mut kcp, &mut recv_tmp, &mut pending_recv);
                check_wait_acks_inner(&kcp, &mut pending_wait_acks);
                check_wait_all_inner(&kcp, &mut pending_wait_all);
                let now_ms = current();
                let next_ms = kcp.check(now_ms);
                let delay_ms = next_ms.saturating_sub(now_ms);
                next_update = tokio::time::Instant::now() + Duration::from_millis(delay_ms as u64);
            }

            _ = shutdown_rx.changed() => {
                break;
            }
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn handle_callback_cmd_deferred(
    kcp: &mut Kcp<impl KcpOutput>,
    cmd: KcpCmd,
    pending_recv: &mut Option<oneshot::Sender<Result<BytesMut>>>,
    pending_wait_acks: &mut Vec<PendingWaitAck>,
    pending_wait_all: &mut Vec<oneshot::Sender<Result<()>>>,
    recv_tmp: &mut BytesMut,
) {
    match cmd {
        KcpCmd::Send { data, ack } => {
            kcp.update(current());
            let r = kcp.send(&data);
            let _ = ack.send(r);
        }
        KcpCmd::Input { data } => {
            kcp.update(current());
            if let Err(e) = kcp.input_bytes(data) {
                log::warn!("KCP input error: {}", e);
            }
        }
        KcpCmd::Recv { ack } => {
            if ack.is_closed() {
                return;
            }
            kcp.update(current());
            if let Some(r) = try_recv_inner_fn(kcp, recv_tmp) {
                let _ = ack.send(r);
            } else {
                *pending_recv = Some(ack);
            }
        }
        KcpCmd::TryRecv { ack } => {
            kcp.update(current());
            if let Some(r) = try_recv_inner_fn(kcp, recv_tmp) {
                let _ = ack.send(r);
            } else {
                let _ = ack.send(Err(KcpError::RecvQueueEmpty));
            }
        }
        KcpCmd::SendWithHandle { data, ack } => {
            kcp.update(current());
            let r = kcp.send_with_handle(&data);
            let _ = ack.send(r);
        }
        KcpCmd::WaitAck { handle, ack } => {
            if kcp.is_send_acked(handle) {
                let _ = ack.send(Ok(()));
            } else if kcp.is_dead() {
                let _ = ack.send(Err(KcpError::DeadLink));
            } else {
                pending_wait_acks.push(PendingWaitAck {
                    handle,
                    deadline: None,
                    ack,
                });
            }
        }
        KcpCmd::WaitAckTimeout {
            handle,
            timeout,
            ack,
        } => {
            if kcp.is_send_acked(handle) {
                let _ = ack.send(Ok(()));
            } else if kcp.is_dead() {
                let _ = ack.send(Err(KcpError::DeadLink));
            } else {
                pending_wait_acks.push(PendingWaitAck {
                    handle,
                    deadline: Some(tokio::time::Instant::now() + timeout),
                    ack,
                });
            }
        }
        KcpCmd::WaitAllSent { ack } => {
            if kcp.wait_snd() == 0 {
                let _ = ack.send(Ok(()));
            } else if kcp.is_dead() {
                let _ = ack.send(Err(KcpError::DeadLink));
            } else {
                pending_wait_all.push(ack);
            }
        }
        KcpCmd::IsDead { ack } => {
            let _ = ack.send(kcp.is_dead());
        }
        KcpCmd::WaitSnd { ack } => {
            let _ = ack.send(kcp.wait_snd());
        }
        KcpCmd::SendReconnect { ack } => {
            kcp.update(current());
            let r = kcp.send_reconnect();
            let _ = ack.send(r);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn callback_post_batch<Output: KcpOutput + Send + 'static>(
    kcp: &mut Kcp<impl KcpOutput>,
    collected: &Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    output: &Arc<std::sync::Mutex<Option<Output>>>,
    pending_recv: &mut Option<oneshot::Sender<Result<BytesMut>>>,
    pending_wait_acks: &mut Vec<PendingWaitAck>,
    pending_wait_all: &mut Vec<oneshot::Sender<Result<()>>>,
    recv_tmp: &mut BytesMut,
) {
    kcp.flush();
    drain_callback_output(collected, output);
    try_wake_recv_inner(kcp, recv_tmp, pending_recv);
    check_wait_acks_inner(kcp, pending_wait_acks);
    check_wait_all_inner(kcp, pending_wait_all);
}

fn try_recv_inner_fn(
    kcp: &mut Kcp<impl KcpOutput>,
    recv_tmp: &mut BytesMut,
) -> Option<Result<BytesMut>> {
    match kcp.peek_size() {
        Ok(size) => {
            recv_tmp.clear();
            recv_tmp.resize(size, 0);
            match kcp.recv(recv_tmp) {
                Ok(n) => Some(Ok(BytesMut::from(&recv_tmp[..n]))),
                Err(e) => Some(Err(e)),
            }
        }
        Err(KcpError::RecvQueueEmpty) | Err(KcpError::IncompletePacket) => None,
        Err(e) => Some(Err(e)),
    }
}

fn try_wake_recv_inner(
    kcp: &mut Kcp<impl KcpOutput>,
    recv_tmp: &mut BytesMut,
    pending_recv: &mut Option<oneshot::Sender<Result<BytesMut>>>,
) {
    if let Some(ack) = pending_recv.take() {
        if ack.is_closed() {
            return;
        }
        if let Some(r) = try_recv_inner_fn(kcp, recv_tmp) {
            let _ = ack.send(r);
        } else {
            *pending_recv = Some(ack);
        }
    }
}

fn check_wait_acks_inner(
    kcp: &Kcp<impl KcpOutput>,
    pending: &mut Vec<PendingWaitAck>,
) {
    let now = tokio::time::Instant::now();
    let mut to_resolve: Vec<(usize, Result<()>)> = Vec::new();
    for (i, p) in pending.iter().enumerate() {
        if kcp.is_send_acked(p.handle) {
            to_resolve.push((i, Ok(())));
        } else if kcp.is_dead() {
            to_resolve.push((i, Err(KcpError::DeadLink)));
        } else if p.deadline.is_some_and(|d| now >= d) {
            to_resolve.push((i, Err(KcpError::Timeout)));
        }
    }
    for (i, result) in to_resolve.into_iter().rev() {
        let p = pending.remove(i);
        let _ = p.ack.send(result);
    }
}

fn check_wait_all_inner(
    kcp: &Kcp<impl KcpOutput>,
    pending: &mut Vec<oneshot::Sender<Result<()>>>,
) {
    if kcp.wait_snd() == 0 {
        for p in pending.drain(..) {
            let _ = p.send(Ok(()));
        }
    } else if kcp.is_dead() {
        for p in pending.drain(..) {
            let _ = p.send(Err(KcpError::DeadLink));
        }
    }
}

fn drain_callback_output<Output: KcpOutput + Send + 'static>(
    collected: &Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    output: &Arc<std::sync::Mutex<Option<Output>>>,
) {
    let packets: Vec<Vec<u8>> = collected.lock().unwrap_or_else(std::sync::PoisonError::into_inner).drain(..).collect();
    if let Some(ref callback) = *output.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
        for pkt in &packets {
            callback(pkt);
        }
    }
}

//! KCP Actor — 单 task 独占 Kcp 实例，直接 socket 发送

use bytes::{Bytes, BytesMut};
use crossbeam_queue::ArrayQueue;
use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};

use kcp2_core::{Kcp, KcpOutput, current, KcpError, Result};

use super::cmd::{KcpCmd, PendingWaitAck};
use crate::crypto::KcpCrypto;
use crate::transport::{BatchSendResult, KcpTransport};

const OUTPUT_QUEUE_CAPACITY: usize = 32;
const OUTPUT_POOL_CAPACITY: usize = 16;
const DEFAULT_PENDING_SEND_CAP: usize = 64;

/// KCP output 收集器 — flush 时将数据包收集到锁无关的 ArrayQueue，使用 BytesMut 池减少分配
type OutputCollector = Box<dyn Fn(&[u8]) + Send + Sync>;

fn make_output_collector(
    pool: Arc<ArrayQueue<BytesMut>>,
) -> (OutputCollector, Arc<ArrayQueue<BytesMut>>) {
    let queue: Arc<ArrayQueue<BytesMut>> = Arc::new(ArrayQueue::new(OUTPUT_QUEUE_CAPACITY));
    let q = queue.clone();
    let collector: OutputCollector = Box::new(move |data: &[u8]| {
        let mut buf = pool.pop().unwrap_or_default();
        buf.clear();
        buf.extend_from_slice(data);
        if q.push(buf).is_err() {
            log::warn!("KCP output queue full ({}), packet dropped", OUTPUT_QUEUE_CAPACITY);
        }
    });
    (collector, queue)
}

/// KCP Actor — 单 task 独占 Kcp 实例
///
/// 接收 `KcpCmd` 命令，直接操作 Kcp，通过 socket 发送数据。
/// 使用 `ikcp_check()` 精确定时 update。
pub(crate) struct KcpActor {
    kcp: Kcp<OutputCollector>,
    transport: Arc<dyn KcpTransport>,
    peer: SocketAddr,
    cmd_rx: mpsc::Receiver<KcpCmd>,
    shutdown_rx: watch::Receiver<bool>,
    connected: bool,
    collected: Arc<ArrayQueue<BytesMut>>,
    output_pool: Arc<ArrayQueue<BytesMut>>,
    crypto: Option<Arc<dyn KcpCrypto>>,
    pending_recv: Option<oneshot::Sender<Result<BytesMut>>>,
    pending_wait_acks: Vec<PendingWaitAck>,
    pending_wait_all: Vec<oneshot::Sender<Result<()>>>,
    pending_send: VecDeque<Vec<u8>>,
    pending_send_cap: usize,
    recv_tmp: BytesMut,
}

impl KcpActor {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        conv: u32,
        transport: Arc<dyn KcpTransport>,
        peer: SocketAddr,
        connected: bool,
        cmd_rx: mpsc::Receiver<KcpCmd>,
        shutdown_rx: watch::Receiver<bool>,
        crypto: Option<Arc<dyn KcpCrypto>>,
        pending_send_cap: usize,
    ) -> Self {
        let output_pool = Arc::new(ArrayQueue::new(OUTPUT_POOL_CAPACITY));
        let (collector, collected) = make_output_collector(output_pool.clone());
        let mut kcp = Kcp::new(conv, collector);
        kcp.update(0);

        Self {
            kcp,
            transport,
            peer,
            cmd_rx,
            shutdown_rx,
            connected,
            collected,
            output_pool,
            crypto,
            pending_recv: None,
            pending_wait_acks: Vec::new(),
            pending_wait_all: Vec::new(),
            pending_send: VecDeque::new(),
            pending_send_cap: if pending_send_cap > 0 { pending_send_cap } else { DEFAULT_PENDING_SEND_CAP },
            recv_tmp: BytesMut::with_capacity(128),
        }
    }

    pub fn kcp_mut(&mut self) -> &mut Kcp<impl KcpOutput> {
        &mut self.kcp
    }

    /// Actor 主循环
    pub(crate) async fn run(mut self) {
        let mut next_update = self.calc_next_update();

        loop {
            tokio::select! {
                biased;

                // 优先处理命令
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(c) => {
                            if self.handle_cmd(c) {
                                break;
                            }
                        }
                        None => break,
                    }
                    next_update = self.calc_next_update();
                }

                // 精确定时 update
                _ = tokio::time::sleep_until(next_update) => {
                    self.do_update();
                    next_update = self.calc_next_update();
                }

                // 关闭信号（值变为 true 或 sender drop 均触发退出）
                _ = self.shutdown_rx.changed() => {
                    break;
                }
            }
        }
    }

    // ─── 命令处理 ──────────────────────────────────────

    /// 返回 true 表示 Actor 应退出
    #[allow(clippy::too_many_lines)]
    fn handle_cmd(&mut self, cmd: KcpCmd) -> bool {
        match cmd {
            KcpCmd::Send { data, ack } => {
                self.kcp.update(current());
                let result = self.kcp.send(&data);
                self.flush_and_drain();
                let _ = ack.send(result);
            }

            KcpCmd::Input { data } => {
                self.kcp.update(current());
                let data = if let Some(ref crypto) = self.crypto {
                    match crypto.decrypt(&data) {
                        Some(plaintext) => Bytes::from(plaintext),
                        None => {
                            log::warn!("KCP crypto: auth failed, packet discarded");
                            return false;
                        }
                    }
                } else {
                    data
                };
                if self.kcp.input_bytes(data).is_ok() {
                    self.flush_and_drain();
                }
                self.try_wake_recv();
                self.check_wait_acks();
                self.check_wait_all();
            }

            KcpCmd::Recv { ack } => {
                self.kcp.update(current());
                if ack.is_closed() {
                    // recv future was cancelled (e.g. tokio::select! picked another
                    // branch). Don't consume data from KCP queue — next Recv will get it.
                    return false;
                }
                if let Some(data) = self.try_recv_inner() {
                    self.flush_and_drain();
                    if ack.send(data).is_err() {
                        // Receiver dropped between is_closed() and send().
                        // Data consumed from KCP queue but receiver gone.
                        // Extremely unlikely race, but data is lost.
                    }
                } else {
                    self.pending_recv = Some(ack);
                }
            }

            KcpCmd::TryRecv { ack } => {
                self.kcp.update(current());
                if let Some(data) = self.try_recv_inner() {
                    self.flush_and_drain();
                    let _ = ack.send(data);
                } else {
                    let _ = ack.send(Err(KcpError::RecvQueueEmpty));
                }
            }

            KcpCmd::SendWithHandle { data, ack } => {
                self.kcp.update(current());
                let result = self.kcp.send_with_handle(&data);
                self.flush_and_drain();
                let _ = ack.send(result);
            }

            KcpCmd::WaitAck { handle, ack } => {
                if self.kcp.is_send_acked(handle) {
                    let _ = ack.send(Ok(()));
                } else if self.kcp.is_dead() {
                    let _ = ack.send(Err(KcpError::DeadLink));
                } else {
                    self.pending_wait_acks.push(PendingWaitAck {
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
                if self.kcp.is_send_acked(handle) {
                    let _ = ack.send(Ok(()));
                } else if self.kcp.is_dead() {
                    let _ = ack.send(Err(KcpError::DeadLink));
                } else {
                    self.pending_wait_acks.push(PendingWaitAck {
                        handle,
                        deadline: Some(tokio::time::Instant::now() + timeout),
                        ack,
                    });
                }
            }

            KcpCmd::WaitAllSent { ack } => {
                if self.kcp.wait_snd() == 0 {
                    let _ = ack.send(Ok(()));
                } else if self.kcp.is_dead() {
                    let _ = ack.send(Err(KcpError::DeadLink));
                } else {
                    self.pending_wait_all.push(ack);
                }
            }

            KcpCmd::IsDead { ack } => {
                let _ = ack.send(self.kcp.is_dead());
            }

            KcpCmd::WaitSnd { ack } => {
                let _ = ack.send(self.kcp.wait_snd());
            }

            KcpCmd::IsSendAcked { handle, ack } => {
                let _ = ack.send(self.kcp.is_send_acked(handle));
            }

            KcpCmd::Kill => {
                self.kcp.kill();
                self.pending_send.clear();
                self.resolve_all_pending_with_dead();
                return true;
            }

            KcpCmd::SendReconnect { ack } => {
                self.pending_send.clear();
                self.kcp.update(current());
                let result = self.kcp.send_reconnect();
                self.flush_and_drain();
                let _ = ack.send(result);
            }

            KcpCmd::ResetRto => {
                self.kcp.update(current());
                self.kcp.reset_rto();
                self.flush_and_drain();
            }
        }
        false
    }

    // ─── 内部操作 ──────────────────────────────────────

    fn do_update(&mut self) {
        self.kcp.update(current());
        self.flush_and_drain();
        self.try_wake_recv();
        self.check_wait_acks();
        self.check_wait_all();
    }

    fn flush_and_drain(&mut self) {
        self.kcp.flush();
        self.retry_pending();
        self.drain_output();
    }

    fn drain_output(&mut self) {
        if self.transport.supports_batch_send() {
            self.drain_output_batch();
        } else {
            self.drain_output_per_packet();
        }
    }

    fn drain_output_batch(&mut self) {
        let mut payloads: Vec<Vec<u8>> = Vec::new();
        let mut orig_pkts: Vec<BytesMut> = Vec::new();

        while let Some(pkt) = self.collected.pop() {
            let payload: Vec<u8> = if let Some(ref crypto) = self.crypto {
                crypto.encrypt(self.kcp.conv(), &pkt)
            } else {
                pkt.to_vec()
            };
            orig_pkts.push(pkt);
            payloads.push(payload);
        }

        if payloads.is_empty() {
            return;
        }

        let slices: Vec<&[u8]> = payloads.iter().map(Vec::as_slice).collect();
        let result = if self.connected {
            self.transport.try_send_batch_connected(&slices)
        } else {
            self.transport.try_send_batch_to(&slices, self.peer)
        };

        match result {
            Ok(BatchSendResult::All(_)) => {
                for pkt in orig_pkts {
                    let _ = self.output_pool.push(pkt);
                }
            }
            Ok(BatchSendResult::Partial { sent, .. }) => {
                for pkt in orig_pkts.into_iter().take(sent) {
                    let _ = self.output_pool.push(pkt);
                }
                for payload in payloads.into_iter().skip(sent) {
                    if self.pending_send.len() < self.pending_send_cap {
                        self.pending_send.push_back(payload);
                    } else {
                        log::warn!("KCP pending_send full, dropping packet");
                    }
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                for payload in payloads {
                    if self.pending_send.len() < self.pending_send_cap {
                        self.pending_send.push_back(payload);
                    } else {
                        log::warn!("KCP pending_send full, dropping packet");
                    }
                }
                for pkt in orig_pkts {
                    let _ = self.output_pool.push(pkt);
                }
            }
            Err(e) => {
                log::warn!("KCP batch send error: {}", e);
                for pkt in orig_pkts {
                    let _ = self.output_pool.push(pkt);
                }
            }
        }
    }

    fn drain_output_per_packet(&mut self) {
        while let Some(pkt) = self.collected.pop() {
            let payload: Vec<u8> = if let Some(ref crypto) = self.crypto {
                crypto.encrypt(self.kcp.conv(), &pkt)
            } else {
                pkt.to_vec()
            };
            let result = if self.connected {
                self.transport.try_send(&payload)
            } else {
                self.transport.try_send_to(&payload, self.peer)
            };
            match result {
                Ok(_) => {
                    let _ = self.output_pool.push(pkt);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if self.pending_send.len() < self.pending_send_cap {
                        self.pending_send.push_back(payload);
                    } else {
                        log::warn!("KCP pending_send full, dropping packet");
                    }
                    break;
                }
                Err(e) => {
                    log::warn!("KCP output send error: {}", e);
                    let _ = self.output_pool.push(pkt);
                }
            }
        }
    }

    /// 重试暂存的 WouldBlock 包
    fn retry_pending(&mut self) {
        if self.pending_send.is_empty() {
            return;
        }

        if self.transport.supports_batch_send() {
            self.retry_pending_batch();
        } else {
            self.retry_pending_per_packet();
        }
    }

    fn retry_pending_batch(&mut self) {
        let total = self.pending_send.len().min(OUTPUT_QUEUE_CAPACITY);
        let items: Vec<&[u8]> = self.pending_send.iter().take(total).map(Vec::as_slice).collect();

        let result = if self.connected {
            self.transport.try_send_batch_connected(&items)
        } else {
            self.transport.try_send_batch_to(&items, self.peer)
        };

        match result {
            Ok(BatchSendResult::All(_)) => {
                self.pending_send.drain(..total);
            }
            Ok(BatchSendResult::Partial { sent, .. }) => {
                self.pending_send.drain(..sent);
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => {
                log::warn!("KCP pending batch send error: {}", e);
                self.pending_send.pop_front();
            }
        }
    }

    fn retry_pending_per_packet(&mut self) {
        while !self.pending_send.is_empty() {
            let pkt = &self.pending_send[0];
            let result = if self.connected {
                self.transport.try_send(pkt)
            } else {
                self.transport.try_send_to(pkt, self.peer)
            };
            match result {
                Ok(_) => {
                    self.pending_send.pop_front();
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    break;
                }
                Err(e) => {
                    log::warn!("KCP pending send error: {}", e);
                    self.pending_send.pop_front();
                }
            }
        }
    }

    /// 尝试接收一个完整消息
    fn try_recv_inner(&mut self) -> Option<Result<BytesMut>> {
        // 先 peek 大小
        match self.kcp.peek_size() {
            Ok(size) => {
                self.recv_tmp.clear();
                self.recv_tmp.reserve(size);
                // SAFETY: kcp.recv() writes up to `size` bytes, returns actual count
                // We only read up to `n` bytes, so uninitialized tail is never accessed
                #[allow(unsafe_code)]
                unsafe { self.recv_tmp.set_len(size); }
                match self.kcp.recv(&mut self.recv_tmp) {
                    Ok(n) => Some(Ok(BytesMut::from(&self.recv_tmp[..n]))),
                    Err(e) => Some(Err(e)),
                }
            }
            Err(KcpError::RecvQueueEmpty) | Err(KcpError::IncompletePacket) => None,
            Err(e) => Some(Err(e)),
        }
    }

    /// 尝试唤醒挂起的 recv
    fn try_wake_recv(&mut self) {
        if let Some(ack) = self.pending_recv.take() {
            if ack.is_closed() {
                return;
            }
            if let Some(result) = self.try_recv_inner() {
                let _ = ack.send(result);
            } else {
                self.pending_recv = Some(ack);
            }
        }
    }

    /// 检查所有挂起的 wait_ack 请求
    fn check_wait_acks(&mut self) {
        let now = tokio::time::Instant::now();
        let mut i = 0;
        while i < self.pending_wait_acks.len() {
            let pending = &self.pending_wait_acks[i];
            let resolved = if self.kcp.is_send_acked(pending.handle) {
                Some(Ok(()))
            } else if self.kcp.is_dead() {
                Some(Err(KcpError::DeadLink))
            } else if pending.deadline.is_some_and(|d| now >= d) {
                Some(Err(KcpError::Timeout))
            } else {
                None
            };
            if let Some(result) = resolved {
                let pending = self.pending_wait_acks.remove(i);
                let _ = pending.ack.send(result);
            } else {
                i += 1;
            }
        }
    }

    /// 检查所有挂起的 wait_all_sent 请求
    fn check_wait_all(&mut self) {
        if self.kcp.wait_snd() == 0 {
            for pending in self.pending_wait_all.drain(..) {
                let _ = pending.send(Ok(()));
            }
        } else if self.kcp.is_dead() {
            for pending in self.pending_wait_all.drain(..) {
                let _ = pending.send(Err(KcpError::DeadLink));
            }
        }
    }

    /// Kill 后清理所有挂起请求
    fn resolve_all_pending_with_dead(&mut self) {
        if let Some(ack) = self.pending_recv.take() {
            let _ = ack.send(Err(KcpError::DeadLink));
        }
        for pending in self.pending_wait_acks.drain(..) {
            let _ = pending.ack.send(Err(KcpError::DeadLink));
        }
        for pending in self.pending_wait_all.drain(..) {
            let _ = pending.send(Err(KcpError::DeadLink));
        }
        self.pending_send.clear();
    }

    /// 使用 ikcp_check 计算下次 update 时间
    fn calc_next_update(&self) -> tokio::time::Instant {
        let now_ms = current();
        let next_update_ms = self.kcp.check(now_ms);
        // check() returns an absolute timestamp (ms since epoch), not a delay.
        // When update is needed immediately, it returns `current`.
        // Convert to a Duration relative to now.
        let delay_ms = next_update_ms.saturating_sub(now_ms);
        tokio::time::Instant::now() + Duration::from_millis(delay_ms as u64)
    }
}

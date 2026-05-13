use super::common::*;
use super::types::{KcpOutput, LinkState, SendHandle, SendResult};
use crate::consts::*;
use crate::errors::{KcpError, Result};
use crate::segment::Segment;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::{max, min};

const ACKLIST_LIMIT: usize = 1024;

pub struct Kcp<Output: KcpOutput> {
    conv: u32,
    mtu: usize,
    mss: usize,
    state: LinkState,
    snd_una: u32,
    snd_nxt: u32,
    rcv_nxt: u32,
    ssthresh: u16,
    rx_rttval: u32,
    rx_srtt: u32,
    rx_rto: u32,
    rx_minrto: u32,
    snd_wnd: u16,
    rcv_wnd: u16,
    rmt_wnd: u16,
    cwnd: u16,
    probe: u32,
    current: u32,
    interval: u32,
    ts_flush: u32,
    xmit: u32,
    nodelay: bool,
    updated: bool,
    ts_probe: u32,
    probe_wait: u32,
    dead_link: u32,
    incr: u32,
    snd_queue: VecDeque<Segment>,
    rcv_queue: VecDeque<Segment>,
    snd_buf: Vec<(u32, Segment)>,
    rcv_buf: BTreeMap<u32, Segment>,
    acklist: Vec<(u32, u32)>,
    buffer: Vec<u8>,
    fastresend: u32,
    fastlimit: u32,
    nocwnd: bool,
    stream: bool,
    output: Output,
    next_sn_for_handle: u32,
    is_fresh: bool,
    free_segments: Vec<Segment>,
}

impl<Output: KcpOutput> Kcp<Output> {
    pub fn new(conv: u32, output: Output) -> Self {
        Self {
            conv,
            mtu: MTU_DEF,
            mss: MTU_DEF - OVERHEAD,
            state: LinkState::Active,
            snd_una: 0,
            snd_nxt: 0,
            rcv_nxt: 0,
            ssthresh: THRESH_INIT,
            rx_rttval: 0,
            rx_srtt: 0,
            rx_rto: RTO_DEF,
            rx_minrto: RTO_MIN,
            snd_wnd: WND_SND,
            rcv_wnd: WND_RCV,
            rmt_wnd: WND_RCV,
            cwnd: 0,
            probe: 0,
            current: 0,
            interval: INTERVAL,
            ts_flush: INTERVAL,
            xmit: 0,
            nodelay: false,
            updated: false,
            ts_probe: 0,
            probe_wait: 0,
            dead_link: DEADLINK,
            incr: 0,
            snd_queue: VecDeque::new(),
            rcv_queue: VecDeque::new(),
            snd_buf: Vec::new(),
            rcv_buf: BTreeMap::new(),
            acklist: Vec::with_capacity(256),
            buffer: vec![0u8; (MTU_DEF + OVERHEAD) * 3],
            fastresend: 0,
            fastlimit: FASTACK_LIMIT,
            nocwnd: false,
            stream: false,
            output,
            next_sn_for_handle: 0,
            is_fresh: true,
            free_segments: Vec::new(),
        }
    }

    pub fn get_conv(data: &[u8]) -> Option<u32> {
        if data.len() < 4 {
            return None;
        }
        Some(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
    }

    pub fn conv(&self) -> u32 {
        self.conv
    }

    pub fn peek_size(&self) -> Result<usize> {
        self.peek_size_internal()
    }

    pub fn set_mtu(&mut self, mtu: usize) -> Result<()> {
        if mtu < 50 || mtu < OVERHEAD {
            return Err(KcpError::MtuTooSmall {
                mtu,
                min: core::cmp::max(50, OVERHEAD),
            });
        }
        self.mtu = mtu;
        self.mss = mtu - OVERHEAD;
        self.buffer = vec![0u8; (mtu + OVERHEAD) * 3];
        Ok(())
    }

    pub fn set_wndsize(&mut self, sndwnd: u16, rcvwnd: u16) {
        if sndwnd > 0 {
            self.snd_wnd = sndwnd;
        }
        if rcvwnd > 0 {
            self.rcv_wnd = max(rcvwnd, WND_RCV);
        }
    }

    pub fn set_nodelay(&mut self, nodelay: bool, interval: u32, resend: u32, nc: bool) {
        self.nodelay = nodelay;
        if nodelay {
            self.rx_minrto = RTO_NDL;
            self.rx_rto = RTO_NDL;
        } else {
            self.rx_minrto = RTO_MIN;
        }
        if interval > 0 {
            self.interval = interval.clamp(10, 5000);
        }
        if resend > 0 {
            self.fastresend = resend;
        }
        self.nocwnd = nc;
    }

    pub fn set_interval(&mut self, interval: u32) {
        self.interval = interval.clamp(10, 5000);
    }

    pub fn set_rx_minrto(&mut self, minrto: u32) {
        self.rx_minrto = minrto;
        if self.rx_rto < minrto {
            self.rx_rto = minrto;
        }
    }

    pub fn set_dead_link(&mut self, dead_link: u32) {
        self.dead_link = dead_link;
    }

    pub fn state(&self) -> i32 {
        if self.state == LinkState::Dead {
            -1
        } else {
            0
        }
    }

    pub fn is_dead(&self) -> bool {
        self.state == LinkState::Dead
    }

    pub fn kill(&mut self) {
        self.state = LinkState::Dead;
    }

    pub fn wait_snd(&self) -> usize {
        self.snd_buf.len() + self.snd_queue.len()
    }

    pub fn reset_rto(&mut self) {
        let rto = self.rx_minrto;
        for (_, seg) in &mut self.snd_buf {
            seg.rto = rto;
            seg.resendts = self.current + rto;
        }
    }

    /// 处理对端发来的 CMD_RECONNECT。
    ///
    /// 根据当前连接状态走两条路径之一：
    /// - **全新连接**：仅记录 `rmt_wnd` 并标记 `is_fresh = false`，不做破坏性操作。
    /// - **重连**（已有发送/接收状态）：完全清空所有缓冲并重置序列号、拥塞控制、
    ///   探测状态，使连接回到初始状态（相当于"软重启"）。
    fn handle_reconnect(&mut self, peer_wnd: u16) {
        let is_fresh = self.rcv_nxt == 0 && self.snd_nxt == 0 && self.state == LinkState::Active;

        if is_fresh {
            log::info!("CMD_RECONNECT from fresh connection, conv={}", self.conv);
        } else {
            log::warn!(
                "reconnect via CMD_RECONNECT, conv={}, old snd_nxt={}, old rcv_nxt={}",
                self.conv,
                self.snd_nxt,
                self.rcv_nxt
            );

            let snd_queue_segs: Vec<Segment> = self.snd_queue.drain(..).collect();
            for seg in snd_queue_segs {
                self.release_segment(seg);
            }
            self.rcv_queue.clear();
            let snd_buf_segs: Vec<(u32, Segment)> = self.snd_buf.drain(..).collect();
            for (_, seg) in snd_buf_segs {
                self.release_segment(seg);
            }
            self.rcv_buf.clear();
            self.acklist.clear();

            self.snd_una = 0;
            self.snd_nxt = 0;
            self.rcv_nxt = 0;
            self.next_sn_for_handle = 0;

            self.state = LinkState::Active;
            self.xmit = 0;

            self.cwnd = 0;
            self.incr = 0;
            self.ssthresh = THRESH_INIT;
            self.probe = 0;
            self.ts_probe = 0;
            self.probe_wait = 0;
        }

        self.rmt_wnd = peer_wnd;
        self.is_fresh = false;
    }

    /// 发送 CMD_RECONNECT 段给对端。
    ///
    /// 构造一个 24 字节的纯头部段（无数据 payload），通过 output 回调发出。
    /// 用于客户端断线重连时通知服务端重置状态。
    ///
    /// 注意：此方法仅将段送入 output 回调，上层应随后调用 `flush()` 确保
    /// 数据实际发出。在 kcp2-std/kcp2-embassy 中，`flush` 由 Actor 自动管理。
    pub fn send_reconnect(&mut self) -> Result<()> {
        let mut seg = self.acquire_segment();
        seg.conv = self.conv;
        seg.cmd = CMD_RECONNECT;
        seg.wnd = self.wnd_unused();
        seg.una = self.rcv_nxt;
        seg.ts = self.current;

        let mut buf = [0u8; OVERHEAD];
        seg.encode_to_slice(&mut buf[..])?;
        (self.output)(&buf);
        Ok(())
    }

    fn peek_size_internal(&self) -> Result<usize> {
        if self.rcv_queue.is_empty() {
            log::trace!("peek_size_internal: recv queue empty");
            return Err(KcpError::RecvQueueEmpty);
        }
        let seg = &self.rcv_queue[0];
        if seg.frg == 0 {
            return Ok(seg.data.len());
        }
        if self.rcv_queue.len() < seg.frg as usize + 1 {
            log::trace!(
                "peek_size_internal: incomplete packet, have {} fragments, need {}",
                self.rcv_queue.len(),
                seg.frg + 1
            );
            return Err(KcpError::IncompletePacket);
        }
        let mut len = 0;
        for seg in &self.rcv_queue {
            len += seg.data.len();
            if seg.frg == 0 {
                break;
            }
        }
        Ok(len)
    }

    fn acquire_segment(&mut self) -> Segment {
        self.free_segments.pop().unwrap_or_default()
    }

    /// Maximum segment pool size to bound memory under high churn.
    const FREE_SEGMENTS_MAX: usize = 64;

    fn release_segment(&mut self, mut seg: Segment) {
        seg.data.clear();
        if self.free_segments.len() < Self::FREE_SEGMENTS_MAX {
            self.free_segments.push(seg);
        }
    }

    fn wnd_unused(&self) -> u16 {
        if self.rcv_queue.len() < self.rcv_wnd as usize {
            self.rcv_wnd - self.rcv_queue.len() as u16
        } else {
            0
        }
    }

    pub fn recv(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.rcv_queue.is_empty() {
            return Err(KcpError::RecvQueueEmpty);
        }

        let first = &self.rcv_queue[0];
        let need_count = (first.frg + 1) as usize;
        if self.rcv_queue.len() < need_count {
            return Err(KcpError::IncompletePacket);
        }

        // Fast path: single segment (most common case, frg == 0)
        if need_count == 1 {
            let seg = &self.rcv_queue[0];
            let len = seg.data.len();
            if len > buf.len() {
                return Err(KcpError::BufferTooSmall {
                    required: len,
                    available: buf.len(),
                });
            }
            buf[..len].copy_from_slice(&seg.data);
            let recover = self.rcv_queue.len() >= self.rcv_wnd as usize;
            self.rcv_queue.drain(..1);
            self.move_buf_to_queue();
            if recover && self.rcv_queue.len() < self.rcv_wnd as usize {
                self.probe |= ASK_TELL;
            }
            return Ok(len);
        }

        // Multi-segment path
        let mut offset = 0;
        let mut count = 0;
        for seg in &self.rcv_queue {
            let end = offset + seg.data.len();
            if end > buf.len() {
                let total: usize = self
                    .rcv_queue
                    .iter()
                    .take(need_count)
                    .map(|s| s.data.len())
                    .sum();
                return Err(KcpError::BufferTooSmall {
                    required: total,
                    available: buf.len(),
                });
            }
            buf[offset..end].copy_from_slice(&seg.data);
            offset = end;
            count += 1;
            if seg.frg == 0 {
                break;
            }
        }

        let recover = self.rcv_queue.len() >= self.rcv_wnd as usize;
        self.rcv_queue.drain(..count);
        self.move_buf_to_queue();
        if recover && self.rcv_queue.len() < self.rcv_wnd as usize {
            self.probe |= ASK_TELL;
        }
        Ok(offset)
    }

    pub fn set_stream(&mut self, stream: bool) {
        self.stream = stream;
    }

    fn send_internal(&mut self, data: &[u8], track_handle: bool) -> Result<SendResult> {
        if data.is_empty() {
            return Err(KcpError::EmptyData);
        }

        let mut buffer = data;
        let mut sent = 0;
        let sn_start = self.next_sn_for_handle;
        let mut tail_extend = 0;

        if self.stream {
            if let Some(old) = self.snd_queue.back_mut() {
                if old.data.len() < self.mss {
                    let capacity = self.mss - old.data.len();
                    let extend = min(buffer.len(), capacity);
                    old.data.extend_from_slice(&buffer[..extend]);
                    buffer = &buffer[extend..];
                    sent += extend;
                    tail_extend = extend;
                }
            }
            if buffer.is_empty() {
                let sn = if track_handle {
                    sn_start.saturating_sub(1)
                } else {
                    0
                };
                return Ok(SendResult {
                    bytes_sent: sent,
                    sn_start: sn,
                    sn_count: 0,
                });
            }
        }

        let count = if buffer.len() <= self.mss {
            1
        } else {
            buffer.len().div_ceil(self.mss)
        };

        if count >= WND_RCV as usize {
            // Roll back tail append to avoid corrupting snd_queue
            if tail_extend > 0 {
                if let Some(old) = self.snd_queue.back_mut() {
                    old.data.truncate(old.data.len() - tail_extend);
                }
            }
            return Err(KcpError::TooManyFragments {
                count,
                max: WND_RCV as usize,
            });
        }

        let count = if count == 0 { 1 } else { count };
        let mut seg_count = 0u32;

        for i in 0..count {
            let size = min(self.mss, buffer.len());
            let mut seg = self.acquire_segment();
            seg.data = Vec::from(&buffer[..size]);
            seg.frg = if self.stream {
                0
            } else {
                (count - i - 1) as u8
            };
            self.snd_queue.push_back(seg);
            buffer = &buffer[size..];
            sent += size;
            seg_count += 1;
        }

        if track_handle {
            self.next_sn_for_handle += seg_count;
        }

        Ok(SendResult {
            bytes_sent: sent,
            sn_start,
            sn_count: seg_count,
        })
    }

    pub fn send(&mut self, data: &[u8]) -> Result<usize> {
        self.send_internal(data, false).map(|r| r.bytes_sent)
    }

    pub fn send_with_handle(&mut self, data: &[u8]) -> Result<SendHandle> {
        let r = self.send_internal(data, true)?;
        if r.sn_count == 0 && self.stream {
            let sn = r.sn_start;
            return Ok(SendHandle {
                sn_start: sn,
                sn_end: sn,
            });
        }
        Ok(SendHandle {
            sn_start: r.sn_start,
            sn_end: r.sn_start + r.sn_count - 1,
        })
    }

    pub fn is_send_acked(&self, handle: SendHandle) -> bool {
        time_diff(self.snd_una, handle.sn_end) > 0
    }

    pub fn snd_una(&self) -> u32 {
        self.snd_una
    }

    fn update_rtt(&mut self, rtt: u32) {
        if self.rx_srtt == 0 {
            self.rx_srtt = rtt;
            self.rx_rttval = rtt / 2;
        } else {
            let delta = rtt.abs_diff(self.rx_srtt);
            self.rx_rttval = ((3u64 * self.rx_rttval as u64 + delta as u64) / 4) as u32;
            self.rx_srtt = ((7u64 * self.rx_srtt as u64 + rtt as u64) / 8) as u32;
            if self.rx_srtt < 1 {
                self.rx_srtt = 1;
            }
        }
        let rto =
            (self.rx_srtt as u64 + max(self.interval as u64, 4 * self.rx_rttval as u64)) as u32;
        self.rx_rto = rto.clamp(self.rx_minrto, RTO_MAX);
    }

    fn parse_una(&mut self, una: u32) {
        let mut i = 0;
        while i < self.snd_buf.len() {
            if time_diff(una, self.snd_buf[i].0) > 0 {
                let (_, seg) = self.snd_buf.remove(i);
                self.release_segment(seg);
            } else {
                i += 1;
            }
        }
    }

    fn parse_ack(&mut self, sn: u32) {
        if time_diff(sn, self.snd_una) < 0 || time_diff(sn, self.snd_nxt) >= 0 {
            return;
        }
        log::trace!("ack sn={}, una={}", sn, self.snd_una);
        if let Ok(idx) = self.snd_buf.binary_search_by_key(&sn, |(k, _)| *k) {
            let (_, seg) = self.snd_buf.remove(idx);
            self.release_segment(seg);
        }
    }

    fn parse_fastack(&mut self, sn: u32, ts: u32) {
        if time_diff(sn, self.snd_una) < 0 || time_diff(sn, self.snd_nxt) >= 0 {
            return;
        }
        log::trace!("fastack sn={}, ts={}", sn, ts);
        for (entry_sn, seg) in &mut self.snd_buf {
            if time_diff(sn, *entry_sn) < 0 {
                break;
            } else if sn != *entry_sn {
                #[cfg(feature = "fastack_conserve")]
                {
                    if time_diff(ts, seg.ts) < 0 {
                        seg.fastack += 1;
                    }
                }
                #[cfg(not(feature = "fastack_conserve"))]
                {
                    seg.fastack += 1;
                }
            }
        }
    }

    fn parse_data(&mut self, new_seg: Segment) {
        let sn = new_seg.sn;

        if self.rcv_nxt == 0 && self.rcv_buf.is_empty() && sn > 0 {
            self.rcv_nxt = sn;
        }

        if time_diff(sn, self.rcv_nxt + self.rcv_wnd as u32) >= 0 || time_diff(sn, self.rcv_nxt) < 0
        {
            return;
        }

        log::trace!(
            "data sn={}, frg={}, expect={}",
            sn,
            new_seg.frg,
            self.rcv_nxt
        );

        if let alloc::collections::btree_map::Entry::Vacant(e) = self.rcv_buf.entry(sn) {
            e.insert(new_seg);
        }
        self.move_buf_to_queue();
    }

    fn move_buf_to_queue(&mut self) {
        loop {
            if self.rcv_queue.len() >= self.rcv_wnd as usize {
                break;
            }
            if let Some(seg) = self.rcv_buf.remove(&self.rcv_nxt) {
                self.rcv_nxt += 1;
                self.rcv_queue.push_back(seg);
                continue;
            }
            break;
        }
    }

    pub fn input(&mut self, data: &[u8]) -> Result<usize> {
        log::trace!("input: {} bytes at time {}", data.len(), self.current);

        let data_len = data.len();
        let old_una = self.snd_una;
        let mut flag = false;
        let mut max_ack: u32 = 0;
        let mut latest_ts: u32 = 0;

        if data_len < OVERHEAD {
            return Err(KcpError::InputTooShort {
                len: data_len,
                min: OVERHEAD,
            });
        }

        let mut offset = 0;
        while offset < data_len {
            let remaining = data_len - offset;
            if remaining < OVERHEAD {
                break;
            }
            let (seg, consumed) = match Segment::decode_from_slice(&data[offset..]) {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("decode error at offset {}: {}", offset, e);
                    break;
                }
            };
            offset += consumed;

            if seg.conv != self.conv {
                return Err(KcpError::ConvMismatch {
                    expected: self.conv,
                    got: seg.conv,
                });
            }

            self.rmt_wnd = seg.wnd;
            self.parse_una(seg.una);

            if self.is_fresh {
                // is_fresh 在 handle_reconnect 中已被清除，
                // 通用 input 路径不应对 snd_nxt/snd_una/rcv_nxt 做修改，
                // 否则当本地已发送数据时（snd_buf 非空），重置 snd_nxt 会导致
                // flush 中 snd_buf.last().sn >= snd_nxt 的断言失败。
                self.is_fresh = false;
            }

            match seg.cmd {
                CMD_ACK => {
                    if time_diff(self.current, seg.ts) >= 0 {
                        self.update_rtt(time_diff(self.current, seg.ts) as u32);
                    }
                    self.parse_ack(seg.sn);
                    if !flag {
                        flag = true;
                        max_ack = seg.sn;
                        latest_ts = seg.ts;
                    } else if time_diff(seg.sn, max_ack) > 0 {
                        max_ack = seg.sn;
                        latest_ts = seg.ts;
                    }
                }
                CMD_PUSH => {
                    if time_diff(seg.sn, self.rcv_nxt + self.rcv_wnd as u32) < 0 {
                        if self.acklist.len() < ACKLIST_LIMIT {
                            self.acklist.push((seg.sn, seg.ts));
                        } else {
                            log::warn!("acklist full, dropping ACK for sn={}", seg.sn);
                        }
                        if time_diff(seg.sn, self.rcv_nxt) >= 0 {
                            self.parse_data(seg);
                        }
                    }
                }
                CMD_WASK => {
                    self.probe |= ASK_TELL;
                }
                CMD_WINS => {}
                CMD_RECONNECT => {
                    self.handle_reconnect(seg.wnd);
                }
                _ => return Err(KcpError::InvalidCmd { cmd: seg.cmd }),
            }
        }

        self.snd_una = self
            .snd_buf
            .first()
            .map(|(sn, _)| *sn)
            .unwrap_or(self.snd_nxt);

        if flag {
            self.parse_fastack(max_ack, latest_ts);
        }

        if time_diff(self.snd_una, old_una) > 0 && self.cwnd < self.rmt_wnd {
            let (cwnd, _ssthresh, incr) = update_congestion(
                self.snd_una,
                old_una,
                self.cwnd,
                self.ssthresh,
                self.rmt_wnd,
                self.mss,
                self.incr,
            );
            self.cwnd = cwnd;
            self.incr = incr;
        }

        Ok(offset)
    }

    #[cfg(feature = "bytes")]
    pub fn input_bytes(&mut self, data: bytes::Bytes) -> Result<usize> {
        self.input(&data)
    }

    #[cfg(not(feature = "bytes"))]
    pub fn input_bytes(&mut self, data: &[u8]) -> Result<usize> {
        self.input(data)
    }

    #[allow(unused_assignments)]
    /// 不推荐直接调用。请使用 `update()`，内部会自动在合适的时机执行 flush。
    #[doc(hidden)]
    pub fn flush(&mut self) {
        if !self.updated {
            return;
        }

        let current = self.current;
        let mut seg = self.acquire_segment();
        seg.conv = self.conv;
        seg.cmd = CMD_ACK;
        seg.wnd = self.wnd_unused();
        seg.una = self.rcv_nxt;

        let mut ptr = 0;

        #[allow(unused_assignments)]
        macro_rules! emit {
            ($ptr:expr) => {
                if $ptr > 0 {
                    (self.output)(&self.buffer[..$ptr]);
                    $ptr = 0;
                }
            };
        }

        let acklist_len = self.acklist.len();
        for i in 0..acklist_len {
            let (sn, ts) = self.acklist[i];
            if ptr + OVERHEAD > self.mtu {
                emit!(ptr);
            }
            seg.sn = sn;
            seg.ts = ts;
            seg.encode_to_slice(&mut self.buffer[ptr..]).unwrap();
            ptr += OVERHEAD;
        }
        self.acklist.clear();

        if self.rmt_wnd == 0 {
            if self.probe_wait == 0 {
                self.probe_wait = PROBE_INIT;
                self.ts_probe = current + self.probe_wait;
            } else if time_diff(current, self.ts_probe) >= 0 {
                if self.probe_wait < PROBE_INIT {
                    self.probe_wait = PROBE_INIT;
                }
                self.probe_wait += self.probe_wait / 2;
                if self.probe_wait > PROBE_LIMIT {
                    self.probe_wait = PROBE_LIMIT;
                }
                self.ts_probe = current + self.probe_wait;
                self.probe |= ASK_SEND;
            }
        } else {
            self.ts_probe = 0;
            self.probe_wait = 0;
        }

        if (self.probe & ASK_SEND) != 0 {
            seg.cmd = CMD_WASK;
            if ptr + OVERHEAD > self.mtu {
                emit!(ptr);
            }
            seg.encode_to_slice(&mut self.buffer[ptr..]).unwrap();
            ptr += OVERHEAD;
        }

        if (self.probe & ASK_TELL) != 0 {
            seg.cmd = CMD_WINS;
            if ptr + OVERHEAD > self.mtu {
                emit!(ptr);
            }
            seg.encode_to_slice(&mut self.buffer[ptr..]).unwrap();
            ptr += OVERHEAD;
        }

        self.probe = 0;

        let mut cwnd = min(self.snd_wnd, self.rmt_wnd);
        if !self.nocwnd {
            cwnd = min(cwnd, self.cwnd);
        }

        while time_diff(self.snd_nxt, self.snd_una + cwnd as u32) < 0 {
            if let Some(mut newseg) = self.snd_queue.pop_front() {
                newseg.conv = self.conv;
                newseg.cmd = CMD_PUSH;
                newseg.wnd = seg.wnd;
                newseg.ts = self.current;
                newseg.sn = self.snd_nxt;
                newseg.una = self.rcv_nxt;
                newseg.resendts = self.current;
                newseg.rto = self.rx_rto;
                newseg.fastack = 0;
                newseg.xmit = 0;
                debug_assert!(self.snd_buf.last().map_or(true, |(last_sn, _)| *last_sn < self.snd_nxt));
                self.snd_buf.push((self.snd_nxt, newseg));
                self.snd_nxt += 1;
                if self.next_sn_for_handle < self.snd_nxt {
                    self.next_sn_for_handle = self.snd_nxt;
                }
            } else {
                break;
            }
        }

        let resent = if self.fastresend > 0 {
            self.fastresend
        } else {
            u32::MAX
        };
        let rtomin = if self.nodelay { 0 } else { self.rx_rto >> 3 };

        let mut lost = false;
        let mut change = false;

        for (_, seg) in &mut self.snd_buf {
            let decision = check_resend(
                seg.xmit,
                seg.resendts,
                seg.fastack,
                self.current,
                resent,
                self.fastlimit,
            );

            match decision {
                ResendDecision::FirstSend => {
                    seg.xmit += 1;
                    seg.rto = calculate_rto(self.nodelay, self.rx_rto, self.rx_minrto);
                    seg.resendts = self.current + seg.rto + rtomin;
                }
                ResendDecision::Timeout => {
                    seg.xmit += 1;
                    self.xmit += 1;
                    log::trace!("retransmit sn={}, xmit={}", seg.sn, seg.xmit);
                    seg.rto = update_rto_for_retransmit(self.nodelay, seg.rto, self.rx_rto);
                    seg.resendts = self.current + seg.rto;
                    lost = true;
                }
                ResendDecision::FastRetransmit => {
                    seg.xmit += 1;
                    seg.fastack = 0;
                    seg.resendts = self.current + seg.rto;
                    log::trace!("fast retransmit sn={}", seg.sn);
                    change = true;
                }
                ResendDecision::NoResend => continue,
            }

            seg.ts = self.current;
            seg.una = self.rcv_nxt;
        }

        for (_, seg) in &self.snd_buf {
            let need = OVERHEAD + seg.data.len();
            if ptr + need > self.mtu {
                emit!(ptr);
            }

            seg.encode_to_slice(&mut self.buffer[ptr..]).unwrap();
            ptr += need;

            if seg.xmit >= self.dead_link && self.state != LinkState::Dead {
                log::warn!(
                    "dead link detected after {} retransmits on sn={}",
                    seg.xmit,
                    seg.sn
                );
                self.state = LinkState::Dead;
            }
        }

        emit!(ptr);

        if change {
            let (cwnd, ssthresh, incr) =
                congestion_fast_retransmit(self.snd_nxt, self.snd_una, resent, self.mss);
            self.cwnd = cwnd;
            self.ssthresh = ssthresh;
            self.incr = incr;
        }

        if lost {
            let (cwnd, ssthresh, incr) = congestion_loss(cwnd, self.mss);
            self.cwnd = cwnd;
            self.ssthresh = ssthresh;
            self.incr = incr;
        }

        if self.cwnd < 1 {
            self.cwnd = 1;
            self.incr = self.mss as u32;
        }
    }

    pub fn update(&mut self, current: u32) {
        log::trace!("update: current={}, ts_flush={}", current, self.ts_flush);

        self.current = current;

        if !self.updated {
            self.updated = true;
            self.ts_flush = self.current;
        }

        let mut slap = time_diff(self.current, self.ts_flush);

        if !(-10000..10000).contains(&slap) {
            self.ts_flush = self.current;
            slap = 0;
        }

        if slap >= 0 {
            self.ts_flush += self.interval;
            if time_diff(self.current, self.ts_flush) >= 0 {
                self.ts_flush = self.current + self.interval;
            }
            self.flush();
        }
    }

    pub fn check(&self, current: u32) -> u32 {
        if !self.updated {
            return current;
        }

        let mut flush_target = self.ts_flush;
        if time_diff(current, flush_target) >= 10000 || time_diff(current, flush_target) < -10000 {
            flush_target = current;
        }

        if time_diff(current, flush_target) >= 0 {
            return current;
        }

        let mut tm_packet = u32::MAX;
        for (_, seg) in &self.snd_buf {
            let diff = time_diff(seg.resendts, current);
            if diff <= 0 {
                return current;
            }
            if (diff as u32) < tm_packet {
                tm_packet = diff as u32;
            }
        }

        let flush_diff = time_diff(flush_target, current) as u32;
        min(min(tm_packet, flush_diff), self.interval)
    }
}

use super::common::*;
use super::types::{KcpOutput, LinkState, SendHandle, SendResult};
use crate::consts::*;
use crate::errors::{KcpError, Result};
use crate::segment::Segment;

use core::cmp::{max, min};
use heapless::Vec as HeaplessVec;

pub struct Kcp<Output: KcpOutput, const MAX_SEGMENTS: usize = 32> {
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
    snd_queue: HeaplessVec<Segment, MAX_SEGMENTS>,
    rcv_queue: HeaplessVec<Segment, MAX_SEGMENTS>,
    snd_buf: HeaplessVec<(u32, Segment), MAX_SEGMENTS>,
    rcv_buf: HeaplessVec<(u32, Segment), MAX_SEGMENTS>,
    acklist: HeaplessVec<(u32, u32), MAX_SEGMENTS>,
    buffer: HeaplessVec<u8, 4488>,
    fastresend: u32,
    fastlimit: u32,
    nocwnd: bool,
    stream: bool,
    output: Output,
    next_sn_for_handle: u32,
    is_fresh: bool,
}

impl<Output: KcpOutput, const MAX_SEGMENTS: usize> Kcp<Output, MAX_SEGMENTS> {
    /// 创建并预填充 buffer，避免 encode_to_slice 时 panic
    fn new_buffer() -> HeaplessVec<u8, 4488> {
        let mut v = HeaplessVec::new();
        v.extend_from_slice(&[0u8; 4488]).expect("heapless buffer: fixed-size Vec pre-allocated for 4488 bytes");
        v
    }

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
            snd_queue: HeaplessVec::new(),
            rcv_queue: HeaplessVec::new(),
            snd_buf: HeaplessVec::new(),
            rcv_buf: HeaplessVec::new(),
            acklist: HeaplessVec::new(),
            buffer: Self::new_buffer(),
            fastresend: 0,
            fastlimit: FASTACK_LIMIT,
            nocwnd: false,
            stream: false,
            output,
            next_sn_for_handle: 0,
            is_fresh: true,
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
        let required_buffer = (mtu + OVERHEAD) * 3;
        if required_buffer > 4488 {
            return Err(KcpError::MtuTooSmall {
                mtu,
                min: core::cmp::max(50, OVERHEAD),
            });
        }
        // heapless segment data capacity is 1400 bytes (HeaplessVec<u8, 1400>)
        if mtu > OVERHEAD + 1400 {
            return Err(KcpError::MtuTooSmall {
                mtu,
                min: core::cmp::max(50, OVERHEAD),
            });
        }
        self.mtu = mtu;
        self.mss = mtu - OVERHEAD;
        self.buffer = Self::new_buffer();
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
        for (_, seg) in self.snd_buf.iter_mut() {
            seg.rto = rto;
            seg.resendts = self.current + rto;
        }
    }

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

            self.snd_queue.clear();
            self.rcv_queue.clear();
            self.snd_buf.clear();
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

    pub fn send_reconnect(&mut self) -> Result<()> {
        let mut seg = Segment::new();
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

    fn wnd_unused(&self) -> u16 {
        if self.rcv_queue.len() < self.rcv_wnd as usize {
            self.rcv_wnd - self.rcv_queue.len() as u16
        } else {
            0
        }
    }

    pub fn recv(&mut self, buf: &mut [u8]) -> Result<usize> {
        let size = self.peek_size_internal()?;
        if size > buf.len() {
            return Err(KcpError::BufferTooSmall {
                required: size,
                available: buf.len(),
            });
        }
        let recover = self.rcv_queue.len() >= self.rcv_wnd as usize;
        let mut offset = 0;
        let mut count = 0;
        for seg in &self.rcv_queue {
            buf[offset..offset + seg.data.len()].copy_from_slice(&seg.data);
            offset += seg.data.len();
            count += 1;
            if seg.frg == 0 {
                break;
            }
        }
        // Batch shift: move remaining elements to front (O(n) vs O(n²))
        let len = self.rcv_queue.len();
        if count > 0 && count < len {
            for i in 0..len - count {
                self.rcv_queue[i] = core::mem::replace(
                    &mut self.rcv_queue[i + count],
                    Segment::new(),
                );
            }
            self.rcv_queue.truncate(len - count);
        } else if count >= len {
            self.rcv_queue.clear();
        }
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
            if let Some(old) = self.snd_queue.last_mut() {
                if old.data.len() < self.mss {
                    let capacity = self.mss - old.data.len();
                    let extend = min(buffer.len(), capacity);
                    let mut new_data = HeaplessVec::new();
                    new_data.extend_from_slice(&old.data).map_err(|_| {
                        KcpError::TooManyFragments {
                            count: old.data.len() + extend,
                            max: self.mss,
                        }
                    })?;
                    new_data.extend_from_slice(&buffer[..extend]).map_err(|_| {
                        KcpError::TooManyFragments {
                            count: old.data.len() + extend,
                            max: self.mss,
                        }
                    })?;
                    old.data = new_data;
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
            if tail_extend > 0 {
                if let Some(old) = self.snd_queue.last_mut() {
                    let original_len = old.data.len() - tail_extend;
                    let mut rolled_back = HeaplessVec::new();
                    // tail_extend > 0 implies extend_from_slice succeeded above,
                    // so original_len <= self.mss and this can't fail
                    let _ = rolled_back.extend_from_slice(&old.data[..original_len]);
                    old.data = rolled_back;
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
            let mut seg = Segment::new();
            seg.data = HeaplessVec::new();
            seg.data.extend_from_slice(&buffer[..size]).map_err(|_| {
                KcpError::TooManyFragments {
                    count: size,
                    max: self.mss,
                }
            })?;
            seg.frg = if self.stream {
                0
            } else {
                (count - i - 1) as u8
            };
            self.snd_queue
                .push(seg)
                .map_err(|_| KcpError::TooManyFragments {
                    count: self.snd_queue.len() + 1,
                    max: MAX_SEGMENTS,
                })?;
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
        self.snd_buf.retain(|(sn, _)| time_diff(una, *sn) <= 0);
    }

    fn parse_ack(&mut self, sn: u32) {
        if time_diff(sn, self.snd_una) < 0 || time_diff(sn, self.snd_nxt) >= 0 {
            return;
        }
        log::trace!("ack sn={}, una={}", sn, self.snd_una);
        if let Ok(pos) = self.snd_buf.binary_search_by_key(&sn, |(k, _)| *k) {
            self.snd_buf.remove(pos);
        }
    }

    fn parse_fastack(&mut self, sn: u32, ts: u32) {
        if time_diff(sn, self.snd_una) < 0 || time_diff(sn, self.snd_nxt) >= 0 {
            return;
        }
        log::trace!("fastack sn={}, ts={}", sn, ts);
        for (_, seg) in self.snd_buf.iter_mut() {
            if time_diff(sn, seg.sn) < 0 {
                break;
            } else if sn != seg.sn {
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

        if !self.rcv_buf.iter().any(|(s, _)| *s == sn) && self.rcv_buf.push((sn, new_seg)).is_err()
        {
            log::warn!("rcv_buf full, dropping segment sn={}", sn);
        }
        self.move_buf_to_queue();
    }

    fn move_buf_to_queue(&mut self) {
        loop {
            if self.rcv_queue.len() >= self.rcv_wnd as usize {
                break;
            }
            if let Some(pos) = self.rcv_buf.iter().position(|(s, _)| *s == self.rcv_nxt) {
                let (_, seg) = self.rcv_buf.remove(pos);
                self.rcv_nxt += 1;
                if self.rcv_queue.push(seg).is_err() {
                    log::warn!("rcv_queue full, dropping segment sn={}", self.rcv_nxt - 1);
                }
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

            // snd_buf is sorted by SN (pushed in order, removed by retain/remove)
            self.snd_una = self.snd_buf.first().map(|(sn, _)| *sn).unwrap_or(self.snd_nxt);

            match seg.cmd {
                CMD_ACK => {
                    if time_diff(self.current, seg.ts) >= 0 {
                        self.update_rtt(time_diff(self.current, seg.ts) as u32);
                    }
                    self.parse_ack(seg.sn);
                    self.snd_una = self.snd_buf.first().map(|(sn, _)| *sn).unwrap_or(self.snd_nxt);
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
                        if self.acklist.push((seg.sn, seg.ts)).is_err() {
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
                CMD_WINS => {
                    // Window probe response — window info is already tracked via the wnd field
                    // in each segment header. No additional action needed (matches original KCP behavior).
                }
                CMD_RECONNECT => {
                    self.handle_reconnect(seg.wnd);
                }
                _ => return Err(KcpError::InvalidCmd { cmd: seg.cmd }),
            }
        }

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

    fn output(&self, data: &[u8]) {
        if !data.is_empty() {
            (self.output)(data);
        }
    }

    /// 不推荐直接调用。请使用 `update()`，内部会自动在合适的时机执行 flush。
    #[doc(hidden)]
    pub fn flush(&mut self) {
        if !self.updated {
            return;
        }

        let current = self.current;
        let mut seg = Segment::new();
        seg.conv = self.conv;
        seg.cmd = CMD_ACK;
        seg.wnd = self.wnd_unused();
        seg.una = self.rcv_nxt;

        let mut ptr = 0;

        let acklist = core::mem::take(&mut self.acklist);
        for &(sn, ts) in &acklist {
            if ptr + OVERHEAD > self.mtu {
                self.output(&self.buffer[..ptr]);
                ptr = 0;
            }
            seg.sn = sn;
            seg.ts = ts;
            seg.encode_to_slice(&mut self.buffer[ptr..]).expect("ACK segment encode: buffer guaranteed to fit by MTU check");
            ptr += OVERHEAD;
        }

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
                self.output(&self.buffer[..ptr]);
                ptr = 0;
            }
            seg.encode_to_slice(&mut self.buffer[ptr..]).expect("WASK segment encode: buffer guaranteed to fit by MTU check");
            ptr += OVERHEAD;
        }

        if (self.probe & ASK_TELL) != 0 {
            seg.cmd = CMD_WINS;
            if ptr + OVERHEAD > self.mtu {
                self.output(&self.buffer[..ptr]);
                ptr = 0;
            }
            seg.encode_to_slice(&mut self.buffer[ptr..]).expect("WINS segment encode: buffer guaranteed to fit by MTU check");
            ptr += OVERHEAD;
        }

        self.probe = 0;

        let mut cwnd = min(self.snd_wnd, self.rmt_wnd);
        if !self.nocwnd {
            cwnd = min(cwnd, self.cwnd);
        }

        while time_diff(self.snd_nxt, self.snd_una + cwnd as u32) < 0 {
            if let Some(mut newseg) = self.snd_queue.pop() {
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
                if self.snd_buf.push((self.snd_nxt, newseg)).is_err() {
                    log::warn!("snd_buf full, dropping segment sn={}", self.snd_nxt);
                }
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

        for (_, seg) in self.snd_buf.iter_mut() {
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

        for (_, seg) in self.snd_buf.iter() {
            let need = OVERHEAD + seg.data.len();
            if ptr + need > self.mtu {
                self.output(&self.buffer[..ptr]);
                ptr = 0;
            }

            seg.encode_to_slice(&mut self.buffer[ptr..]).expect("PUSH segment encode: buffer guaranteed to fit by MTU check");
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

        if ptr > 0 {
            self.output(&self.buffer[..ptr]);
        }

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
        for (_, seg) in self.snd_buf.iter() {
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

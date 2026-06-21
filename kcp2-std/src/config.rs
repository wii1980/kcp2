use std::sync::Arc;
use std::time::Duration;

use crate::crypto::KcpCrypto;

pub struct KcpConfig {
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
    pub timeout: Duration,
    pub(crate) crypto: Option<Arc<dyn KcpCrypto>>,
    pub channel_capacity: usize,
    pub max_wait_snd: usize,
    pub pending_send_cap: usize,
    /// Maximum number of concurrent connections. 0 = unlimited (default).
    pub max_connections: usize,
    /// Size of the KCP internal output queue (default: 64).
    ///
    /// When KCP flushes segments faster than the socket can send them,
    /// segments are queued here.  If the queue is full, segments are
    /// dropped and rely on KCP retransmission (ARQ) for recovery.
    ///
    /// Increase this for bursty traffic or large payloads to reduce
    /// retransmission-induced latency spikes.
    pub output_queue_size: usize,
}

impl Default for KcpConfig {
    fn default() -> Self {
        Self {
            nodelay: false,
            interval: 100,
            resend: 0,
            nc: false,
            sndwnd: 32,
            rcvwnd: 128,
            mtu: 1400,
            rx_minrto: 100,
            dead_link: 10,
            stream: false,
            timeout: Duration::from_secs(30),
            crypto: None,
            channel_capacity: 16,
            max_wait_snd: 0,
            pending_send_cap: 64,
            max_connections: 0,
            output_queue_size: 64,
        }
    }
}

impl KcpConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// 启用 AES-256-GCM / ChaCha20-Poly1305 加密
    ///
    /// MTU 扣除由 `effective_mtu()` 统一处理，避免重复扣减。
    pub fn crypto(mut self, crypto: Arc<dyn KcpCrypto>) -> Self {
        self.crypto = Some(crypto);
        self
    }

    /// 获取实际 KCP 层可用的 MTU（自动扣除加密 overhead）
    pub fn effective_mtu(&self) -> usize {
        self.crypto
            .as_ref()
            .map_or(self.mtu, |c| self.mtu.saturating_sub(c.overhead()))
    }

    pub fn nodelay(mut self, nodelay: bool, interval: u32, resend: u32, nc: bool) -> Self {
        self.nodelay = nodelay;
        self.interval = interval;
        self.resend = resend;
        self.nc = nc;
        self
    }

    pub fn wndsize(mut self, sndwnd: u16, rcvwnd: u16) -> Self {
        self.sndwnd = sndwnd;
        self.rcvwnd = rcvwnd;
        self
    }

    pub fn mtu(mut self, mtu: usize) -> Self {
        self.mtu = mtu;
        self
    }

    pub fn rx_minrto(mut self, rx_minrto: u32) -> Self {
        self.rx_minrto = rx_minrto;
        self
    }

    pub fn dead_link(mut self, dead_link: u32) -> Self {
        self.dead_link = dead_link;
        self
    }

    pub fn stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    /// Connection idle timeout and `recv()` timeout.
    ///
    /// Controls two mechanisms:
    /// - **recv() timeout**: `recv()` returns `Err(Timeout)` if no data arrives
    ///   within this duration. The connection stays alive — retry `recv()` to
    ///   wait again.
    /// - **idle timeout**: a background task force-closes the connection if no
    ///   I/O activity occurs for this duration, resolving pending operations
    ///   with `Err(DeadLink)`.
    ///
    /// `Duration::ZERO` disables the recv() timeout (waits indefinitely).
    /// The idle timeout task always runs regardless.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn channel_capacity(mut self, cap: usize) -> Self {
        self.channel_capacity = cap.max(4);
        self
    }

    pub fn max_wait_snd(mut self, max: usize) -> Self {
        self.max_wait_snd = max;
        self
    }

    pub fn pending_send_cap(mut self, cap: usize) -> Self {
        self.pending_send_cap = cap;
        self
    }

    pub fn max_connections(mut self, max: usize) -> Self {
        self.max_connections = max;
        self
    }

    /// Set the KCP internal output queue size (default: 64).
    ///
    /// Larger values reduce retransmissions under bursty write patterns
    /// at the cost of higher memory usage per connection.
    pub fn output_queue_size(mut self, size: usize) -> Self {
        self.output_queue_size = size;
        self
    }
}

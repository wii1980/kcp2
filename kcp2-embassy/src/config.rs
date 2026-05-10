/// Embassy 环境 KCP 配置
#[derive(Debug, Clone)]
pub struct EmbKcpConfig {
    /// 是否启用 nodelay 模式
    pub nodelay: bool,
    /// 内部时钟间隔（毫秒）
    pub interval: u32,
    /// 快速重传阈值（0=禁用）
    pub resend: u32,
    /// 是否禁用拥塞控制
    pub nc: bool,
    /// 发送窗口大小
    pub sndwnd: u16,
    /// 接收窗口大小
    pub rcvwnd: u16,
    /// 最大传输单元
    pub mtu: usize,
    /// 最小 RTO（毫秒）
    pub rx_minrto: u32,
    /// 最大重传次数
    pub dead_link: u32,
    /// 是否启用流模式
    pub stream: bool,
    /// 连接超时（毫秒）
    pub timeout_ms: u64,
    /// update 间隔（毫秒）
    pub update_interval_ms: u64,
}

impl Default for EmbKcpConfig {
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
            timeout_ms: 30_000,
            update_interval_ms: 10,
        }
    }
}

impl EmbKcpConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// 高延迟网络配置（卫星链路、跨国连接）
    pub fn high_latency() -> Self {
        Self {
            nodelay: true,
            interval: 150,
            resend: 2,
            nc: false,
            sndwnd: 512,
            rcvwnd: 512,
            mtu: 1400,
            rx_minrto: 300,
            dead_link: 18,
            stream: false,
            timeout_ms: 60_000,
            update_interval_ms: 10,
        }
    }

    /// 高丢包网络配置（无线、移动网络）
    pub fn high_loss() -> Self {
        Self {
            nodelay: true,
            interval: 80,
            resend: 1,
            nc: true,
            sndwnd: 256,
            rcvwnd: 256,
            mtu: 1400,
            rx_minrto: 80,
            dead_link: 10,
            stream: false,
            timeout_ms: 30_000,
            update_interval_ms: 10,
        }
    }

    /// 低延迟配置（内网、同城）
    pub fn low_latency() -> Self {
        Self {
            nodelay: true,
            interval: 10,
            resend: 2,
            nc: true,
            sndwnd: 512,
            rcvwnd: 512,
            mtu: 1400,
            rx_minrto: 30,
            dead_link: 8,
            stream: false,
            timeout_ms: 10_000,
            update_interval_ms: 5,
        }
    }

    /// ESP32 内存受限配置（小窗口、小 MTU）
    pub fn embedded_constrained() -> Self {
        Self {
            nodelay: true,
            interval: 50,
            resend: 2,
            nc: true,
            sndwnd: 16,
            rcvwnd: 16,
            mtu: 512,
            rx_minrto: 100,
            dead_link: 10,
            stream: false,
            timeout_ms: 30_000,
            update_interval_ms: 10,
        }
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

    pub fn timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }
}

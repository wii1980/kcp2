pub trait KcpOutput: Fn(&[u8]) {}
impl<F: Fn(&[u8])> KcpOutput for F {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkState {
    #[default]
    Active,
    Dead,
}

/// 发送句柄，用于追踪一批 segment 的 ACK 状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendHandle {
    pub sn_start: u32,
    pub sn_end: u32,
}

pub(crate) struct SendResult {
    pub bytes_sent: usize,
    pub sn_start: u32,
    pub sn_count: u32,
}

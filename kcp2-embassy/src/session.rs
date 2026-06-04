use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use embassy_net::udp::UdpSocket;
use embassy_net::IpEndpoint;
use embassy_time::{Duration, Instant};
use kcp2_core::{Kcp, Result as KcpResult, KcpError, Clock};

use crate::config::EmbKcpConfig;
use crate::crypto::EmbKcpCrypto;
use crate::EmbassyClock;

type KcpOutputFn = Box<dyn Fn(&[u8])>;

pub struct EmbKcpSession<'a> {
    kcp: Kcp<KcpOutputFn>,
    pending: Rc<RefCell<Vec<Vec<u8>>>>,
    socket: UdpSocket<'a>,
    remote: IpEndpoint,
    clock: EmbassyClock,
    config: EmbKcpConfig,
    crypto: Option<Box<dyn EmbKcpCrypto>>,
}

impl<'a> EmbKcpSession<'a> {
    pub fn new(
        conv: u32,
        socket: UdpSocket<'a>,
        remote: IpEndpoint,
        config: EmbKcpConfig,
    ) -> Self {
        Self::new_with_crypto(conv, socket, remote, config, None)
    }

    pub fn new_with_crypto(
        conv: u32,
        socket: UdpSocket<'a>,
        remote: IpEndpoint,
        config: EmbKcpConfig,
        crypto: Option<Box<dyn EmbKcpCrypto>>,
    ) -> Self {
        let pending: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
        let pending_for_kcp = pending.clone();
        let output: KcpOutputFn = Box::new(move |data: &[u8]| {
            pending_for_kcp.borrow_mut().push(data.to_vec());
        });
        let kcp = Kcp::new(conv, output);

        let effective_mtu = if let Some(ref c) = crypto {
            config.mtu.saturating_sub(c.overhead())
        } else {
            config.mtu
        };

        let mut session = Self {
            kcp,
            pending,
            socket,
            remote,
            clock: EmbassyClock,
            config,
            crypto,
        };

        session.kcp.set_nodelay(
            session.config.nodelay,
            session.config.interval,
            session.config.resend,
            session.config.nc,
        );
        session.kcp.set_wndsize(session.config.sndwnd, session.config.rcvwnd);
        if let Err(e) = session.kcp.set_mtu(effective_mtu) {
            log::warn!(
                "EmbKcpSession: set_mtu({}) failed: {:?}. KCP MTU unchanged",
                effective_mtu, e
            );
        }
        session.kcp.set_rx_minrto(session.config.rx_minrto);
        session.kcp.set_dead_link(session.config.dead_link);
        session.kcp.set_stream(session.config.stream);

        let ts = session.clock.now_ms();
        session.kcp.update(ts);

        session
    }

    pub fn conv(&self) -> u32 {
        self.kcp.conv()
    }

    pub fn is_dead(&self) -> bool {
        self.kcp.is_dead()
    }

    pub fn send(&mut self, data: &[u8]) -> KcpResult<usize> {
        let ts = self.clock.now_ms();
        self.kcp.update(ts);
        self.kcp.send(data)
    }

    /// 发送 CMD_RECONNECT 段通知对端重置连接状态。
    ///
    /// 用于客户端断线后以相同 `conv` 重连时，通知服务端清空过期状态并重置序列号。
    /// 段仅含 24 字节头部，不携带数据。实际发送由后续 `step()` 或 `flush_and_send()` 完成。
    ///
    /// 注意：此命令是 `kcp2` 的自定义扩展，与标准 KCP 协议不兼容。
    /// 对端也必须是 `kcp2` 实现才能正确处理。
    pub fn send_reconnect(&mut self) -> KcpResult<()> {
        let ts = self.clock.now_ms();
        self.kcp.update(ts);
        self.kcp.send_reconnect()
    }

    pub async fn send_and_flush(&mut self, data: &[u8]) -> KcpResult<usize> {
        let n = self.send(data)?;
        self.flush_and_send().await;
        Ok(n)
    }

    pub fn input(&mut self, data: &[u8]) -> KcpResult<usize> {
        let ts = self.clock.now_ms();
        self.kcp.update(ts);
        self.kcp.input(data)
    }

    pub fn try_recv(&mut self, buf: &mut [u8]) -> KcpResult<usize> {
        self.kcp.recv(buf)
    }

    pub async fn recv(&mut self, buf: &mut [u8]) -> KcpResult<usize> {
        let deadline = Instant::now() + Duration::from_millis(self.config.timeout_ms);
        loop {
            match self.kcp.recv(buf) {
                Ok(n) => return Ok(n),
                Err(KcpError::RecvQueueEmpty) | Err(KcpError::IncompletePacket) => {
                    if Instant::now() >= deadline {
                        return Err(KcpError::Timeout);
                    }
                    self.step().await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub async fn step(&mut self) -> bool {
        let ts = self.clock.now_ms();
        let next_update_ms = self.kcp.check(ts);
        let delay_ms = next_update_ms.saturating_sub(ts).max(1);
        let deadline = Instant::now() + Duration::from_millis(delay_ms as u64);

        let mut recv_buf = [0u8; 1500];

        let result = embassy_futures::select::select(
            async { self.socket.recv_from(&mut recv_buf).await },
            embassy_time::Timer::at(deadline),
        )
        .await;

        match result {
            embassy_futures::select::Either::First(Ok((n, _meta))) => {
                let ts = self.clock.now_ms();
                self.kcp.update(ts);

                let data = &recv_buf[..n];
                if let Some(ref crypto) = self.crypto {
                    match crypto.decrypt(data) {
                        Some(plaintext) => {
                            if let Err(e) = self.kcp.input(&plaintext) {
                                log::warn!("KCP input error (crypto): {:?}", e);
                            }
                        }
                        None => {
                            log::warn!("KCP crypto: auth failed, packet discarded");
                            self.flush_and_send().await;
                            return true;
                        }
                    }
                } else {
                    if let Err(e) = self.kcp.input(data) {
                        log::warn!("KCP input error: {:?}", e);
                    }
                }

                self.flush_and_send().await;
                true
            }
            embassy_futures::select::Either::First(Err(_)) => false,
            embassy_futures::select::Either::Second(()) => {
                let ts = self.clock.now_ms();
                self.kcp.update(ts);
                self.flush_and_send().await;
                false
            }
        }
    }

    async fn flush_and_send(&mut self) {
        self.kcp.flush();
        let packets: Vec<Vec<u8>> = self.pending.borrow_mut().drain(..).collect();
        for pkt in packets {
            let payload = if let Some(ref crypto) = self.crypto {
                crypto.encrypt(self.kcp.conv(), &pkt)
            } else {
                pkt
            };
            if let Err(e) = self.socket.send_to(&payload, self.remote).await {
                log::warn!("KCP send_to failed: {:?}", e);
            }
        }
    }

}

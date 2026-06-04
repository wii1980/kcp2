use std::io;
use std::net::SocketAddr;

use binger_udp::batch::{RecvBatchRaw, SendBatchRaw};
use binger_udp::BingerUdp;

use super::{BatchRecvSlot, BatchSendResult, KcpTransport, RecvFuture, RecvFromFuture};

/// KCP transport backed by binger-udp's batch-capable UDP socket
///
/// Provides drop-in replacement for `UdpTransport` with batch send/recv optimizations.
/// On Linux, uses `sendmmsg`/`recvmmsg` to reduce syscall overhead by ~90%.
pub struct BingerTransport {
    inner: BingerUdp,
    remote: Option<SocketAddr>,
}

impl BingerTransport {
    /// Create from a `BingerUdp` instance
    pub fn new(inner: BingerUdp) -> Self {
        Self { inner, remote: None }
    }

    /// Create with a pre-connected remote address
    pub fn with_remote(inner: BingerUdp, remote: SocketAddr) -> Self {
        Self { inner, remote: Some(remote) }
    }

    /// Access the underlying `BingerUdp`
    pub fn inner(&self) -> &BingerUdp {
        &self.inner
    }
}

impl std::fmt::Debug for BingerTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BingerTransport")
            .field("local_addr", &self.inner.local_addr().ok())
            .field("remote", &self.remote)
            .finish()
    }
}

impl KcpTransport for BingerTransport {
    fn try_send(&self, buf: &[u8]) -> io::Result<usize> {
        if let Some(remote) = self.remote {
            self.inner.try_send_to(buf, remote)
        } else {
            self.inner.try_send_to(buf, "0.0.0.0:0".parse().expect("static SocketAddr literal is valid"))
                .map(|_| buf.len())
        }
    }

    fn try_send_to(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize> {
        self.inner.try_send_to(buf, target)
    }

    fn recv<'a>(&'a self, buf: &'a mut [u8]) -> RecvFuture<'a> {
        Box::pin(async move {
            let (n, _) = self.inner.recv_from(buf).await?;
            Ok(n)
        })
    }

    fn recv_from<'a>(&'a self, buf: &'a mut [u8]) -> RecvFromFuture<'a> {
        Box::pin(self.inner.recv_from(buf))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn try_send_batch_to(
        &self,
        packets: &[&[u8]],
        target: SocketAddr,
    ) -> io::Result<BatchSendResult> {
        let mut batch = SendBatchRaw::with_capacity(packets.len());
        for buf in packets {
            batch.push(buf, Some(target)).map_err(|e| {
                io::Error::new(io::ErrorKind::Other, e.to_string())
            })?;
        }
        let sent = self.inner.try_send_batch(&mut batch)?;
        if sent == packets.len() {
            Ok(BatchSendResult::All(sent))
        } else {
            Ok(BatchSendResult::Partial {
                sent,
                remaining: packets.len() - sent,
            })
        }
    }

    fn try_send_batch_connected(&self, packets: &[&[u8]]) -> io::Result<BatchSendResult> {
        let mut batch = SendBatchRaw::with_capacity(packets.len());
        for buf in packets {
            batch.push(buf, None).map_err(|e| {
                io::Error::new(io::ErrorKind::Other, e.to_string())
            })?;
        }
        let sent = self.inner.try_send_batch(&mut batch)?;
        if sent == packets.len() {
            Ok(BatchSendResult::All(sent))
        } else {
            Ok(BatchSendResult::Partial {
                sent,
                remaining: packets.len() - sent,
            })
        }
    }

    fn try_recv_from_multi(&self, slots: &mut [BatchRecvSlot<'_>]) -> io::Result<usize> {
        if slots.is_empty() {
            return Ok(0);
        }
        let buf_size = slots[0].buf.len();
        let mut batch = RecvBatchRaw::with_capacity(slots.len(), buf_size);
        let n = self.inner.try_recv_batch(&mut batch)?;
        for i in 0..n {
            let data = batch.data(i);
            let len = data.len().min(slots[i].buf.len());
            slots[i].buf[..len].copy_from_slice(&data[..len]);
            slots[i].n = len;
            slots[i].addr = batch.addr(i);
        }
        Ok(n)
    }

    fn supports_batch_send(&self) -> bool {
        true
    }
}

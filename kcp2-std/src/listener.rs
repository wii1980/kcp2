use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use dashmap::DashMap;
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::interval;

use crate::buffer_pool::BufferPool;
use crate::config::KcpConfig;
use crate::connection::KcpConnection;
use crate::reaper::ConnectionReaper;
use crate::transport::{KcpTransport, UdpTransport};

const RECV_BUF_SIZE: usize = 8192;
const BUF_POOL_CAPACITY: usize = 64;

pub struct KcpListener {
    transport: Arc<dyn KcpTransport>,
    connections: Arc<DashMap<u32, Arc<KcpConnection>>>,
    config: KcpConfig,
    next_conv: Arc<parking_lot::Mutex<u32>>,
    reaper: Arc<ConnectionReaper>,
    buf_pool: Arc<BufferPool>,
    cleanup_task: JoinHandle<()>,
    closed: Arc<Notify>,
}

impl KcpListener {
    pub async fn bind(addr: &str) -> io::Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        Self::from_socket(socket, KcpConfig::default())
    }

    pub async fn bind_with_config(addr: &str, config: KcpConfig) -> io::Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        Self::from_socket(socket, config)
    }

    /// 使用自定义传输层创建 Listener
    pub fn from_transport(transport: Arc<dyn KcpTransport>, config: KcpConfig) -> io::Result<Self> {
        Ok(Self::new(transport, config))
    }

    /// 使用 UdpSocket 创建 Listener（向后兼容）
    pub fn from_socket(socket: UdpSocket, config: KcpConfig) -> io::Result<Self> {
        let transport = Arc::new(UdpTransport::new(socket));
        Ok(Self::new(transport, config))
    }

    fn new(transport: Arc<dyn KcpTransport>, config: KcpConfig) -> Self {
        let connections: Arc<DashMap<u32, Arc<KcpConnection>>> = Arc::new(DashMap::with_shard_amount(16));
        let next_conv = Arc::new(parking_lot::Mutex::new(1));
        let closed = Arc::new(Notify::new());
        let reaper = Arc::new(ConnectionReaper::new(config.timeout));

        let connections_clone = connections.clone();
        let reaper_clone = reaper.clone();
        let closed_clone = closed.clone();

        let cleanup_task = tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(5));
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        reaper_clone.run_with_cleanup(&connections_clone, |_| {});
                    }
                    _ = closed_clone.notified() => {
                        break;
                    }
                }
            }
        });

        let buf_pool = Arc::new(BufferPool::new(BUF_POOL_CAPACITY, RECV_BUF_SIZE));

        Self {
            transport,
            connections,
            config,
            next_conv,
            reaper,
            buf_pool,
            cleanup_task,
            closed,
        }
    }

    pub fn allocate_conv(&self) -> u32 {
        let mut next_conv = self.next_conv.lock();
        let conv = *next_conv;
        *next_conv = next_conv.wrapping_add(1).max(1);
        conv
    }

    pub async fn accept(&self) -> io::Result<(Arc<KcpConnection>, SocketAddr)> {
        loop {
            let mut buf = self.buf_pool.get();

            let recv_result = tokio::select! {
                r = self.transport.recv_from(&mut buf) => { r }
                _ = self.closed.notified() => {
                    self.buf_pool.put(buf);
                    return Err(io::Error::other("listener closed"));
                }
            };

            let (n, addr) = match recv_result {
                Ok(r) => r,
                Err(e) => {
                    self.buf_pool.put(buf);
                    return Err(e);
                }
            };

            if n < 4 {
                self.buf_pool.put(buf);
                continue;
            }

            let conv = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);

            let existing_conn = self.connections.get(&conv).map(|r| r.value().clone());

            if let Some(conn) = existing_conn {
                if conn.addr() == addr {
                    let data = Bytes::copy_from_slice(&buf[..n]);
                    self.buf_pool.put(buf);
                    conn.input_bytes(data).await.map_err(|e| {
                        io::Error::other(e.to_string())
                    })?;
                    continue;
                }
                conn.close();
            }

            let conn = self.create_connection(conv, addr);
            let data = Bytes::copy_from_slice(&buf[..n]);
            self.buf_pool.put(buf);
            conn.input_bytes(data).await.map_err(|e| {
                io::Error::other(e.to_string())
            })?;

            return Ok((conn, addr));
        }
    }

    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, Arc<KcpConnection>, SocketAddr)> {
        let mut udp_buf = self.buf_pool.get();

        loop {
            let recv_result = tokio::select! {
                r = self.transport.recv_from(&mut udp_buf) => { r }
                _ = self.closed.notified() => {
                    self.buf_pool.put(udp_buf);
                    return Err(io::Error::other("listener closed"));
                }
            };

            let (n, addr) = match recv_result {
                Ok(r) => r,
                Err(e) => {
                    self.buf_pool.put(udp_buf);
                    return Err(e);
                }
            };

            if n < 4 {
                continue;
            }

            let conv = u32::from_le_bytes([udp_buf[0], udp_buf[1], udp_buf[2], udp_buf[3]]);

            let conn = self.connections.get(&conv).map(|r| r.value().clone());

            let conn = match conn {
                Some(conn) if conn.addr() == addr => {
                    let data = Bytes::copy_from_slice(&udp_buf[..n]);
                    match conn.input_bytes(data).await {
                        Ok(_) => {}
                        Err(e) => {
                            self.buf_pool.put(udp_buf);
                            return Err(io::Error::other(e.to_string()));
                        }
                    }
                    conn
                }
                _ => {
                    // 关闭同 conv 不同地址的旧连接
                    if let Some(old_conn) = self.connections.get(&conv) {
                        if old_conn.addr() != addr {
                            old_conn.close();
                        }
                    }
                    let conn = self.create_connection(conv, addr);
                    let data = Bytes::copy_from_slice(&udp_buf[..n]);
                    match conn.input_bytes(data).await {
                        Ok(_) => {}
                        Err(e) => {
                            self.buf_pool.put(udp_buf);
                            return Err(io::Error::other(e.to_string()));
                        }
                    }
                    conn
                }
            };

            match conn.try_recv(buf).await {
                Ok(size) => {
                    self.buf_pool.put(udp_buf);
                    return Ok((size, conn, addr));
                }
                Err(kcp2_core::KcpError::RecvQueueEmpty) | Err(kcp2_core::KcpError::IncompletePacket) => {
                    continue;
                }
                Err(e) => {
                    self.buf_pool.put(udp_buf);
                    return Err(io::Error::other(e.to_string()));
                }
            }
        }
    }

    pub fn create_connection(&self, conv: u32, addr: SocketAddr) -> Arc<KcpConnection> {
        let conn = Arc::new(KcpConnection::new(
            conv,
            addr,
            &self.config,
            self.transport.clone(),
            false,
        ));

        self.connections.insert(conv, conn.clone());
        self.reaper.touch(conv);

        conn
    }

    pub fn get_connection(&self, conv: u32) -> Option<Arc<KcpConnection>> {
        self.connections.get(&conv).map(|r| r.value().clone())
    }

    pub fn remove_connection(&self, conv: u32) -> Option<Arc<KcpConnection>> {
        self.reaper.remove(conv);
        self.connections.remove(&conv).map(|(_, v)| v)
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.transport.local_addr()
    }

    pub async fn close(&self) {
        self.closed.notify_waiters();
        self.connections.clear();
    }
}

impl Drop for KcpListener {
    fn drop(&mut self) {
        self.cleanup_task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket as StdUdpSocket;

    fn find_free_addr() -> String {
        let sock = StdUdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = sock.local_addr().unwrap();
        drop(sock);
        format!("127.0.0.1:{}", addr.port())
    }

    #[tokio::test]
    async fn test_output_no_spawn() {
        let addr = find_free_addr();
        let listener = KcpListener::bind(&addr).await.unwrap();

        let conv = 1u32;
        let peer_addr: SocketAddr = "127.0.0.1:19999".parse().unwrap();

        let conn = listener.create_connection(conv, peer_addr);
        assert_eq!(conn.conv(), conv);
        assert_eq!(conn.addr(), peer_addr);
        assert_eq!(listener.connection_count(), 1);
    }

    #[tokio::test]
    async fn test_batch_send_ordering() {
        let addr = find_free_addr();
        let listener = KcpListener::bind(&addr).await.unwrap();
        let listener_addr = listener.local_addr().unwrap();
        let _ = listener_addr;

        let recv_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let recv_addr = recv_sock.local_addr().unwrap();

        let conv = 42u32;
        let conn = listener.create_connection(conv, recv_addr);

        let test_data = b"hello batch";
        conn.send(test_data).await.unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut buf = vec![0u8; 2048];
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            recv_sock.recv_from(&mut buf),
        ).await;

        match result {
            Ok(Ok((n, _))) => {
                assert!(n >= 4);
            }
            Ok(Err(e)) => panic!("recv error: {e}"),
            Err(_) => {}
        }
    }

    #[tokio::test]
    async fn test_concurrent_connection_lookup() {
        let addr = find_free_addr();
        let listener = KcpListener::bind(&addr).await.unwrap();

        let num_conns: usize = 100;
        for i in 1..=num_conns {
            let peer: SocketAddr = format!("127.0.0.1:{}", 20000 + i).parse().unwrap();
            listener.create_connection(i as u32, peer);
        }

        assert_eq!(listener.connection_count(), num_conns);

        for i in 1..=num_conns {
            let conn = listener.get_connection(i as u32).unwrap();
            assert_eq!(conn.conv(), i as u32);
        }

        assert!(listener.get_connection(999).is_none());
    }

    #[tokio::test]
    async fn test_reaper_removes_expired() {
        let config = KcpConfig::default().timeout(Duration::from_millis(100));
        let addr = find_free_addr();
        let listener = KcpListener::bind_with_config(&addr, config).await.unwrap();

        let peer: SocketAddr = "127.0.0.1:30000".parse().unwrap();
        listener.create_connection(1, peer);
        listener.create_connection(2, peer);

        assert_eq!(listener.connection_count(), 2);

        tokio::time::sleep(Duration::from_millis(200)).await;

        listener.reaper.run(&listener.connections);

        assert_eq!(listener.connection_count(), 0);
    }
}

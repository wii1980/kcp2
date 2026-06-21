use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::Duration;

use std::collections::HashSet;
use dashmap::DashMap;
use parking_lot::Mutex;

use crate::connection::KcpConnection;

const HEAP_MIN_CAPACITY: usize = 64;

pub(crate) struct ConnectionReaper {
    /// 过期时间堆：(expiry_ms, conv)，最小堆
    expiries: Mutex<BinaryHeap<Reverse<(u64, u32)>>>,
    /// 已移除的 conv 集合（惰性标记）
    removed: Mutex<Vec<u32>>,
    timeout_ms: u32,
}

impl ConnectionReaper {
    pub fn new(timeout: Duration) -> Self {
        Self {
            expiries: Mutex::new(BinaryHeap::with_capacity(HEAP_MIN_CAPACITY)),
            removed: Mutex::new(Vec::new()),
            timeout_ms: timeout.as_millis() as u32,
        }
    }

    pub fn touch(&self, conv: u32) {
        let now_ms = kcp2_core::current() as u64;
        let expiry = now_ms + self.timeout_ms as u64;

        let mut heap = self.expiries.lock();
        {
            let mut removed = self.removed.lock();
            removed.retain(|&c| c != conv);
        }
        heap.push(Reverse((expiry, conv)));
    }

    pub fn remove(&self, conv: u32) {
        self.removed.lock().push(conv);
    }

    /// 当 heap 膨胀时执行清理：移除所有标记为 removed 的条目
    fn gc_if_needed(heap: &mut BinaryHeap<Reverse<(u64, u32)>>, removed: &mut Vec<u32>) {
        if heap.len() < HEAP_MIN_CAPACITY * 2 {
            return;
        }

        // 快速路径：removed 为空无需清理
        if removed.is_empty() {
            return;
        }

        let removed_set: HashSet<u32> = removed.drain(..).collect();

        let old_len = heap.len();
        let mut new_heap = BinaryHeap::with_capacity(old_len / 2);
        while let Some(Reverse((expiry, conv))) = heap.pop() {
            if !removed_set.contains(&conv) {
                new_heap.push(Reverse((expiry, conv)));
            }
        }
        *heap = new_heap;
        drop(removed_set);
    }

    /// 执行超时连接清理，并通过回调执行额外的清理逻辑
    ///
    /// 在移除连接前调用 `conn.close()` 标记 KCP 为死亡状态，
    /// 然后调用 `cleanup(conv)` 由调用方执行自定义清理（如注销调度器）。
    ///
    /// # 参数
    /// - `connections`: 连接表
    /// - `cleanup`: 每个被移除连接的回调，参数为 conv 号
    pub fn run_with_cleanup<F: Fn(u32)>(
        &self,
        connections: &DashMap<u32, Arc<KcpConnection>>,
        cleanup: F,
    ) -> usize {
        let now_ms = kcp2_core::current();
        let mut removed = 0;
        let mut heap = self.expiries.lock();
        let mut removed_list = self.removed.lock();

        while let Some(Reverse((expiry, conv))) = heap.peek().copied() {
            if expiry > now_ms as u64 {
                break;
            }

            heap.pop();

            if removed_list.contains(&conv) {
                removed_list.retain(|&c| c != conv);
                continue;
            }

            let Some(conn) = connections.get(&conv) else {
                continue;
            };

            let elapsed = now_ms.saturating_sub(conn.last_active_millis());
            if elapsed > self.timeout_ms {
                conn.close();
                drop(conn);
                cleanup(conv);
                connections.remove(&conv);
                removed += 1;
            } else {
                let new_expiry = now_ms as u64 + self.timeout_ms as u64;
                heap.push(Reverse((new_expiry, conv)));
            }
        }

        // 惰性 GC：heap 膨胀时清理
        Self::gc_if_needed(&mut heap, &mut removed_list);
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reaper_heap_ordering() {
        let reaper = ConnectionReaper::new(Duration::from_secs(10));
        reaper.touch(1);
        reaper.touch(3);
        reaper.touch(2);

        let heap = reaper.expiries.lock();
        let Reverse((_, first_conv)) = heap.peek().unwrap();
        assert_eq!(*first_conv, 1);
    }

    #[test]
    fn test_reaper_no_expired_connections() {
        let reaper = ConnectionReaper::new(Duration::from_secs(60));
        let connections = DashMap::new();
        reaper.touch(1);
        reaper.touch(2);

        let removed = reaper.run_with_cleanup(&connections, |_| {});
        assert_eq!(removed, 0);
    }

    #[test]
    fn test_reaper_remove() {
        let reaper = ConnectionReaper::new(Duration::from_secs(60));
        reaper.touch(1);
        reaper.touch(2);
        reaper.touch(3);

        reaper.remove(2);

        let removed = reaper.removed.lock();
        assert_eq!(removed.len(), 1);
        assert!(removed.contains(&2));
    }

    #[test]
    fn test_reaper_remove_marked_skipped_during_cleanup() {
        let reaper = ConnectionReaper::new(Duration::from_secs(60));
        reaper.touch(1);
        reaper.touch(2);
        reaper.touch(3);

        reaper.remove(2);

        {
            let removed = reaper.removed.lock();
            assert!(removed.contains(&2));
        }

        let connections = DashMap::new();
        let removed = reaper.run_with_cleanup(&connections, |_| {});
        assert_eq!(removed, 0);
    }

    #[tokio::test]
    async fn test_reaper_expired_connection_cleaned() {
        use tokio::net::UdpSocket;
        use crate::connection::KcpConnection;
        use crate::config::KcpConfig;
        use crate::transport::{KcpTransport, UdpTransport};

        let socket = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let transport: Arc<dyn KcpTransport> = Arc::new(UdpTransport::new(socket));
        let config = KcpConfig::default();

        let conn = Arc::new(KcpConnection::new(42, addr, &config, transport, false));
        let connections = DashMap::new();
        connections.insert(42, conn);

        let reaper = ConnectionReaper::new(Duration::from_millis(1));
        reaper.touch(42);

        let removed = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let removed = reaper.run_with_cleanup(&connections, |_| {});
                if removed > 0 {
                    return removed;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("reaper should have cleaned expired connection within 2s");

        assert_eq!(removed, 1, "expired connection should be removed");
        assert!(!connections.contains_key(&42), "connection should be removed from DashMap");
    }

    #[tokio::test]
    async fn test_reaper_cleanup_callback_called() {
        use tokio::net::UdpSocket;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use crate::connection::KcpConnection;
        use crate::config::KcpConfig;
        use crate::transport::{KcpTransport, UdpTransport};

        let socket = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let transport: Arc<dyn KcpTransport> = Arc::new(UdpTransport::new(socket));
        let config = KcpConfig::default();

        let conn = Arc::new(KcpConnection::new(99, addr, &config, transport, false));
        let connections = DashMap::new();
        connections.insert(99, conn);

        let reaper = ConnectionReaper::new(Duration::from_millis(1));
        reaper.touch(99);

        let counter = AtomicUsize::new(0);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                reaper.run_with_cleanup(&connections, |conv| {
                    assert_eq!(conv, 99);
                    counter.fetch_add(1, Ordering::SeqCst);
                });
                if counter.load(Ordering::SeqCst) > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("cleanup callback should have been called within 2s");

        assert_eq!(counter.load(Ordering::SeqCst), 1, "cleanup callback should be called once");
    }
}

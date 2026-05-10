use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::Duration;

use dashmap::{DashMap, DashSet};
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

        let removed_set: DashSet<u32> = removed.drain(..).collect();

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

    /// 执行超时连接清理
    ///
    /// 在移除连接前调用 `conn.close()` 标记 KCP 为死亡状态，
    /// 确保阻塞在 `recv()` 的业务代码能收到错误并退出。
    #[allow(dead_code)]
    pub fn run(&self, connections: &DashMap<u32, Arc<KcpConnection>>) -> usize {
        self.run_with_cleanup(connections, |_| {})
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
                // 先关闭 KCP 状态，使阻塞在 recv() 的协程收到 DeadLink 错误
                conn.close();
                drop(conn);
                // 调用外部回调执行额外清理（如 scheduler.unregister）
                cleanup(conv);
                connections.remove(&conv);
                removed += 1;
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

        let removed = reaper.run(&connections);
        assert_eq!(removed, 0);
    }
}

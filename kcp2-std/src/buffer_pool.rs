use bytes::BytesMut;
use crossbeam_queue::ArrayQueue;

/// 可重用的 `BytesMut` 缓冲区池。
///
/// 使用 `ArrayQueue` 实现无锁的 get/put 操作。
/// 池满时 `put()` 静默丢弃；池空时 `get()` 回退到堆分配。
pub(crate) struct BufferPool {
    pool: ArrayQueue<BytesMut>,
    buf_size: usize,
}

impl BufferPool {
    /// 创建指定容量和缓冲区大小的池（懒分配，不预填充）。
    pub fn new(capacity: usize, buf_size: usize) -> Self {
        let pool = ArrayQueue::new(capacity);
        // 懒分配：首次 get() 时才分配，put() 归还后复用
        Self { pool, buf_size }
    }

    /// 从池中获取一个缓冲区。池空时堆分配新的。
    pub fn get(&self) -> BytesMut {
        self.pool
            .pop()
            .unwrap_or_else(|| BytesMut::zeroed(self.buf_size))
    }

    /// 归还缓冲区到池中。池满时静默丢弃。
    pub fn put(&self, buf: BytesMut) {
        let _ = self.pool.push(buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_pool_reuse() {
        let pool = BufferPool::new(4, 1024);

        // 取出所有预分配的缓冲区
        let mut bufs = Vec::new();
        for _ in 0..4 {
            let buf = pool.get();
            assert_eq!(buf.capacity(), 1024);
            bufs.push(buf);
        }

        // 池空，get 回退到堆分配
        let fallback = pool.get();
        assert_eq!(fallback.capacity(), 1024);

        // 归还一个缓冲区
        pool.put(bufs.pop().unwrap());

        // 再次获取应从池中拿到
        let reused = pool.get();
        assert_eq!(reused.capacity(), 1024);

        // 归还所有
        for buf in bufs {
            pool.put(buf);
        }
        pool.put(reused);

        // 池满，put 静默丢弃
        pool.put(BytesMut::zeroed(1024));
    }

    #[test]
    fn test_buffer_pool_get_returns_correct_size() {
        let pool = BufferPool::new(2, 2048);
        let buf = pool.get();
        assert_eq!(buf.capacity(), 2048);
    }
}

use std::sync::atomic::{AtomicU32, Ordering};

/// 原子会话ID生成器，生成唯一非零的 u32 conv
pub struct ConvGenerator {
    counter: AtomicU32,
}

impl ConvGenerator {
    pub fn new(start: u32) -> Self {
        let start = if start == 0 { 1 } else { start };
        Self {
            counter: AtomicU32::new(start),
        }
    }

    pub fn next(&self) -> u32 {
        loop {
            let val = self.counter.fetch_add(1, Ordering::Relaxed);
            if val != 0 {
                return val;
            }
            // skip 0, the fetch_add already incremented past it
        }
    }
}

impl Default for ConvGenerator {
    fn default() -> Self {
        Self::new(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sequential_unique() {
        let gen = ConvGenerator::new(1);
        let a = gen.next();
        let b = gen.next();
        let c = gen.next();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn test_skip_zero() {
        let gen = ConvGenerator::new(u32::MAX);
        let first = gen.next();
        assert_eq!(first, u32::MAX);
        let second = gen.next();
        assert_ne!(second, 0);
        assert!(second > 0);
    }

    #[test]
    fn test_start_from_nonzero() {
        let gen = ConvGenerator::new(0);
        let val = gen.next();
        assert_ne!(val, 0);
    }
}

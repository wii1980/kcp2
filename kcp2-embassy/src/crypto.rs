//! KCP 加密层 — Embassy no_std 版
//!
//! 提供可插拔的加密/解密 trait，在 EmbKcpSession 层面透明地加密每个 output 包。
//!
//! 启用 `aead` feature 来获得 AES-256-GCM 和 ChaCha20-Poly1305 实现。
//!
//! # 数据包格式（AEAD）
//!
//! ```text
//! [CONV(4) | NONCE(12) | CIPHERTEXT | AEAD_TAG(16)]
//! ```
//!
//! - `CONV`: KCP 会话 ID，明文保留用于路由
//! - `NONCE`: AEAD 随机数（12 字节，原子计数器递增）
//! - `CIPHERTEXT`: KCP segment(s) 加密数据
//! - `AEAD_TAG`: 认证标签（16 字节）

use alloc::vec::Vec;

/// KCP 加密 trait（no_std 版）
///
/// 与 kcp2-std 的 `KcpCrypto` 功能相同，但不需要 `Send + Sync` bound
/// （Embassy 单线程环境）。
///
/// 实现必须保证：
/// - 加密后的数据包包含 conv 元数据，供路由使用
/// - `encrypt()` 和 `decrypt()` 互为逆操作
/// - `overhead()` 返回加密带来的额外字节数
pub trait EmbKcpCrypto {
    /// 加密一个 KCP output 包
    ///
    /// - `conv`: KCP 会话 ID，需明文输出以便路由
    /// - `plaintext`: KCP segment(s) 原始字节
    ///
    /// 返回加密后的完整 UDP 载荷。
    fn encrypt(&self, conv: u32, plaintext: &[u8]) -> Vec<u8>;

    /// 解密一个收到的 UDP 包
    ///
    /// - `buf`: 收到的完整数据（含明文 CONV + 加密内容）
    ///
    /// 返回 `Some(plaintext)` 认证通过，`None` 认证失败/格式错误。
    fn decrypt(&self, buf: &[u8]) -> Option<Vec<u8>>;

    /// 加密带来的额外字节数（nonce + tag + plaintext conv）
    fn overhead(&self) -> usize;
}

/// 空实现 — 不加密，零开销
impl EmbKcpCrypto for () {
    fn encrypt(&self, _conv: u32, plaintext: &[u8]) -> Vec<u8> {
        plaintext.to_vec()
    }

    fn decrypt(&self, buf: &[u8]) -> Option<Vec<u8>> {
        Some(buf.to_vec())
    }

    fn overhead(&self) -> usize {
        0
    }
}

// ─── AEAD 实现（feature-gated）──────────────────────────────────

#[cfg(feature = "aead")]
mod aead_impl {
    use core::cell::Cell;

    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    use alloc::vec::Vec;

    use super::EmbKcpCrypto;

    const NONCE_SIZE: usize = 12;
    const TAG_SIZE: usize = 16;
    const CONV_SIZE: usize = 4;
    /// AEAD overhead: 4 (conv) + 12 (nonce) + 16 (tag) = 32 bytes
    const AEAD_OVERHEAD: usize = CONV_SIZE + NONCE_SIZE + TAG_SIZE;

    /// AES-256-GCM 加密实现
    ///
    /// 数据包格式: `[CONV(4) | NONCE(12) | CIPHERTEXT | AEAD_TAG(16)]`
    pub struct Aes256GcmCrypto {
        cipher: Aes256Gcm,
        nonce_counter: Cell<u64>,
    }

    impl Aes256GcmCrypto {
        /// 从 32 字节密钥创建加密器
        pub fn new(key: &[u8; 32]) -> Self {
            let key = aes_gcm::Key::<Aes256Gcm>::from_slice(key);
            Self {
                cipher: Aes256Gcm::new(key),
                nonce_counter: Cell::new(0),
            }
        }
    }

    impl EmbKcpCrypto for Aes256GcmCrypto {
        fn encrypt(&self, conv: u32, plaintext: &[u8]) -> Vec<u8> {
            let counter = self.nonce_counter.get();
            let next = counter.wrapping_add(1);
            if next == 0 {
                log::error!("AES-256-GCM nonce counter overflow! Key may be compromised.");
            }
            self.nonce_counter.set(next);
            let mut nonce_bytes = [0u8; NONCE_SIZE];
            nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
            let nonce = Nonce::from_slice(&nonce_bytes);

            let ciphertext = self.cipher.encrypt(nonce, plaintext).unwrap_or_else(|e| {
                log::error!("AES-256-GCM encrypt failed: {:?}", e);
                panic!("AES-256-GCM encryption failure");
            });

            debug_assert!(ciphertext.len() == plaintext.len() + TAG_SIZE);

            let mut packet = Vec::with_capacity(CONV_SIZE + NONCE_SIZE + ciphertext.len());
            packet.extend_from_slice(&conv.to_le_bytes());
            packet.extend_from_slice(&nonce_bytes);
            packet.append(&mut ciphertext);

            packet
        }

        fn decrypt(&self, buf: &[u8]) -> Option<Vec<u8>> {
            if buf.len() < CONV_SIZE + NONCE_SIZE + TAG_SIZE {
                return None;
            }
            let nonce_bytes = &buf[CONV_SIZE..CONV_SIZE + NONCE_SIZE];
            let nonce = Nonce::from_slice(nonce_bytes);
            let ciphertext = &buf[CONV_SIZE + NONCE_SIZE..];

            self.cipher.decrypt(nonce, ciphertext).ok()
        }

        fn overhead(&self) -> usize {
            AEAD_OVERHEAD
        }
    }

    impl core::fmt::Debug for Aes256GcmCrypto {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("Aes256GcmCrypto").finish_non_exhaustive()
        }
    }

    /// ChaCha20-Poly1305 加密实现
    ///
    /// 与 AES-256-GCM 完全相同的包格式和接口，使用不同加密算法。
    /// 纯软件实现，无硬件依赖，适合没有 AES 硬件加速的嵌入式设备。
    ///
    /// 数据包格式: `[CONV(4) | NONCE(12) | CIPHERTEXT | AEAD_TAG(16)]`
    pub struct ChaCha20Poly1305Crypto {
        cipher: chacha20poly1305::ChaCha20Poly1305,
        nonce_counter: Cell<u64>,
    }

    impl ChaCha20Poly1305Crypto {
        /// 从 32 字节密钥创建加密器
        pub fn new(key: &[u8; 32]) -> Self {
            use chacha20poly1305::KeyInit as _;
            let key = chacha20poly1305::Key::from_slice(key);
            Self {
                cipher: chacha20poly1305::ChaCha20Poly1305::new(key),
                nonce_counter: Cell::new(0),
            }
        }
    }

    impl EmbKcpCrypto for ChaCha20Poly1305Crypto {
        fn encrypt(&self, conv: u32, plaintext: &[u8]) -> Vec<u8> {
            let counter = self.nonce_counter.get();
            let next = counter.wrapping_add(1);
            if next == 0 {
                log::error!("ChaCha20-Poly1305 nonce counter overflow! Key may be compromised.");
            }
            self.nonce_counter.set(next);
            let mut nonce_bytes = [0u8; NONCE_SIZE];
            nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
            let nonce = chacha20poly1305::Nonce::from_slice(&nonce_bytes);

            let ciphertext = self.cipher.encrypt(nonce, plaintext).unwrap_or_else(|e| {
                log::error!("ChaCha20-Poly1305 encrypt failed: {:?}", e);
                panic!("ChaCha20-Poly1305 encryption failure");
            });

            let mut packet = Vec::with_capacity(CONV_SIZE + NONCE_SIZE + ciphertext.len());
            packet.extend_from_slice(&conv.to_le_bytes());
            packet.extend_from_slice(&nonce_bytes);
            packet.append(&mut ciphertext);

            packet
        }

        fn decrypt(&self, buf: &[u8]) -> Option<Vec<u8>> {
            if buf.len() < CONV_SIZE + NONCE_SIZE + TAG_SIZE {
                return None;
            }
            let nonce_bytes = &buf[CONV_SIZE..CONV_SIZE + NONCE_SIZE];
            let nonce = chacha20poly1305::Nonce::from_slice(nonce_bytes);
            let ciphertext = &buf[CONV_SIZE + NONCE_SIZE..];

            self.cipher.decrypt(nonce, ciphertext).ok()
        }

        fn overhead(&self) -> usize {
            AEAD_OVERHEAD
        }
    }

    impl core::fmt::Debug for ChaCha20Poly1305Crypto {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("ChaCha20Poly1305Crypto")
                .finish_non_exhaustive()
        }
    }
}

#[cfg(feature = "aead")]
pub use aead_impl::{Aes256GcmCrypto, ChaCha20Poly1305Crypto};

//! KCP 加密层抽象 — 装饰器模式
//!
//! 提供可插拔的加密/解密 trait，在 KCP Actor 层面透明地加密每个 output 包。
//!
//! 启用 `cargo` feature `aead` 来获得 AES-256-GCM 和 ChaCha20-Poly1305 实现。
//! `KcpCrypto` trait 本身是通用加密装饰器抽象，未来可承载非 AEAD 实现
//! （流密码 + 独立 MAC、国密 SM4-GCM、反审查混淆等）。

#![allow(clippy::module_name_repetitions)]
//!
//! # 数据包格式（当前 AEAD 实现）
//!
//! 加密后的 UDP 数据包格式为：
//!
//! ```text
//! [CONV(4) | NONCE(12) | CIPHERTEXT | AEAD_TAG(16)]
//! ```
//!
//! - `CONV`: KCP 会话 ID，明文保留以便 Listener 路由
//! - `NONCE`: AEAD 随机数（12 字节）
//! - `CIPHERTEXT`: KCP segment(s) 的加密数据
//! - `AEAD_TAG`: 认证标签（16 字节），由 AEAD 算法附加
//!
//! # 使用示例
//!
//! ```rust,no_run
//! # #[cfg(feature = "aead")]
//! # mod _inner {
//! # fn main() {
//! use std::sync::Arc;
//! use kcp2_std::crypto::{KcpCrypto, Aes256GcmCrypto};
//! use kcp2_std::KcpConfig;
//!
//! let key = Aes256GcmCrypto::generate_key();
//! let crypto = Arc::new(Aes256GcmCrypto::new(&key));
//!
//! let config = KcpConfig::default()
//!     .crypto(crypto.clone())
//!     .mtu(1400);  // internally deducts crypto.overhead() from MTU
//! # }
//! # }
//! ```

use std::sync::Arc;

#[cfg(feature = "aead")]
mod aead;

#[cfg(feature = "aead")]
pub use self::aead::{Aes256GcmCrypto, ChaCha20Poly1305Crypto};

/// KCP 加密 trait
///
/// 提供一个最小接口，对 KCP Actor 的 output 包进行整包加密。
/// 实现必须保证：
/// - 加密后的数据包包含 conv 元数据，供 Listener 路由
/// - `encrypt()` 和 `decrypt()` 互为逆操作
/// - `overhead()` 返回加密带来的额外字节数
pub trait KcpCrypto: Send + Sync {
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
impl KcpCrypto for () {
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

impl<T: KcpCrypto + ?Sized> KcpCrypto for Arc<T> {
    fn encrypt(&self, conv: u32, plaintext: &[u8]) -> Vec<u8> {
        (**self).encrypt(conv, plaintext)
    }

    fn decrypt(&self, buf: &[u8]) -> Option<Vec<u8>> {
        (**self).decrypt(buf)
    }

    fn overhead(&self) -> usize {
        (**self).overhead()
    }
}

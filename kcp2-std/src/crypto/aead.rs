use std::sync::atomic::{AtomicU64, Ordering};

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};

use super::KcpCrypto;

const NONCE_SIZE: usize = 12;
const TAG_SIZE: usize = 16;
const CONV_SIZE: usize = 4;
/// AES-256-GCM overhead: 4 (conv) + 12 (nonce) + 16 (tag) = 32 bytes
const AES_OVERHEAD: usize = CONV_SIZE + NONCE_SIZE + TAG_SIZE;

/// AES-256-GCM 加密实现
///
/// 数据包格式: `[CONV(4) | NONCE(12) | CIPHERTEXT | AEAD_TAG(16)]`
pub struct Aes256GcmCrypto {
    cipher: Aes256Gcm,
    nonce_counter: AtomicU64,
}

impl Aes256GcmCrypto {
    /// 从 32 字节密钥创建加密器
    pub fn new(key: &[u8; 32]) -> Self {
        let key = aes_gcm::Key::<Aes256Gcm>::from_slice(key);
        Self {
            cipher: Aes256Gcm::new(key),
            nonce_counter: AtomicU64::new(0),
        }
    }

    /// 生成一个随机 32 字节密钥
    pub fn generate_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        getrandom::getrandom(&mut key).expect("RNG failure");
        key
    }
}

impl KcpCrypto for Aes256GcmCrypto {
    fn encrypt(&self, conv: u32, plaintext: &[u8]) -> Vec<u8> {
        let counter = self.nonce_counter.fetch_add(1, Ordering::Relaxed);
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
        let nonce = Nonce::from_slice(&nonce_bytes);

        let mut ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .expect("AES-256-GCM encryption should not fail");

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
        AES_OVERHEAD
    }
}

impl std::fmt::Debug for Aes256GcmCrypto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Aes256GcmCrypto").finish_non_exhaustive()
    }
}

/// ChaCha20-Poly1305 加密实现
///
/// 与 AES-256-GCM 完全相同的包格式和接口，使用不同加密算法。
///
/// 数据包格式: `[CONV(4) | NONCE(12) | CIPHERTEXT | AEAD_TAG(16)]`
pub struct ChaCha20Poly1305Crypto {
    cipher: chacha20poly1305::ChaCha20Poly1305,
    nonce_counter: AtomicU64,
}

impl ChaCha20Poly1305Crypto {
    /// 从 32 字节密钥创建加密器
    pub fn new(key: &[u8; 32]) -> Self {
        use chacha20poly1305::KeyInit as _;
        let key = chacha20poly1305::Key::from_slice(key);
        Self {
            cipher: chacha20poly1305::ChaCha20Poly1305::new(key),
            nonce_counter: AtomicU64::new(0),
        }
    }

    /// 生成一个随机 32 字节密钥
    pub fn generate_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        getrandom::getrandom(&mut key).expect("RNG failure");
        key
    }
}

impl KcpCrypto for ChaCha20Poly1305Crypto {
    fn encrypt(&self, conv: u32, plaintext: &[u8]) -> Vec<u8> {
        let counter = self.nonce_counter.fetch_add(1, Ordering::Relaxed);
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
        let nonce = chacha20poly1305::Nonce::from_slice(&nonce_bytes);

        let mut ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .expect("ChaCha20-Poly1305 encryption should not fail");

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
        AES_OVERHEAD
    }
}

impl std::fmt::Debug for ChaCha20Poly1305Crypto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChaCha20Poly1305Crypto").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aes256_gcm_roundtrip() {
        let key = Aes256GcmCrypto::generate_key();
        let crypto = Aes256GcmCrypto::new(&key);

        let conv = 42u32;
        let plaintext = b"hello kcp with encryption!";

        let encrypted = crypto.encrypt(conv, plaintext);
        let decrypted = crypto.decrypt(&encrypted).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_aes256_gcm_tamper_detected() {
        let key = Aes256GcmCrypto::generate_key();
        let crypto = Aes256GcmCrypto::new(&key);

        let encrypted = crypto.encrypt(1, b"test data");
        let mut tampered = encrypted.clone();
        if let Some(b) = tampered.last_mut() {
            *b ^= 0x01; // flip a bit in the tag or last byte
        }
        assert!(crypto.decrypt(&tampered).is_none());
    }

    #[test]
    fn test_aes256_gcm_short_buffer() {
        let key = Aes256GcmCrypto::generate_key();
        let crypto = Aes256GcmCrypto::new(&key);
        assert!(crypto.decrypt(&[0u8; 4]).is_none());
        assert!(crypto.decrypt(&[0u8; 31]).is_none());
    }

    #[test]
    fn test_aes256_gcm_overhead() {
        let key = Aes256GcmCrypto::generate_key();
        let crypto = Aes256GcmCrypto::new(&key);
        let encrypted = crypto.encrypt(1, &[0u8; 100]);
        assert_eq!(encrypted.len(), 100 + crypto.overhead());
        assert_eq!(crypto.overhead(), 32);
    }

    #[test]
    fn test_chacha20_roundtrip() {
        let key = ChaCha20Poly1305Crypto::generate_key();
        let crypto = ChaCha20Poly1305Crypto::new(&key);

        let conv = 99u32;
        let plaintext = b"chaCha20 test payload!";

        let encrypted = crypto.encrypt(conv, plaintext);
        let decrypted = crypto.decrypt(&encrypted).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_chacha20_tamper_detected() {
        let key = ChaCha20Poly1305Crypto::generate_key();
        let crypto = ChaCha20Poly1305Crypto::new(&key);

        let encrypted = crypto.encrypt(1, b"test");
        let mut tampered = encrypted.clone();
        if let Some(b) = tampered.last_mut() {
            *b ^= 0xFF;
        }
        assert!(crypto.decrypt(&tampered).is_none());
    }

    #[test]
    fn test_chacha20_overhead() {
        let key = ChaCha20Poly1305Crypto::generate_key();
        let crypto = ChaCha20Poly1305Crypto::new(&key);
        let encrypted = crypto.encrypt(1, &[0u8; 100]);
        assert_eq!(encrypted.len(), 100 + crypto.overhead());
        assert_eq!(crypto.overhead(), 32);
    }

    #[test]
    fn test_unique_nonces() {
        let key = Aes256GcmCrypto::generate_key();
        let crypto = Aes256GcmCrypto::new(&key);

        let e1 = crypto.encrypt(1, b"a");
        let e2 = crypto.encrypt(1, b"b");
        // nonce should differ (counter increments)
        assert_ne!(&e1[4..16], &e2[4..16]);
    }

    #[test]
    fn test_conv_preserved_in_plaintext() {
        let key = Aes256GcmCrypto::generate_key();
        let crypto = Aes256GcmCrypto::new(&key);

        let conv = 12345u32;
        let encrypted = crypto.encrypt(conv, b"data");
        let extracted_conv =
            u32::from_le_bytes([encrypted[0], encrypted[1], encrypted[2], encrypted[3]]);
        assert_eq!(extracted_conv, conv);
    }
}

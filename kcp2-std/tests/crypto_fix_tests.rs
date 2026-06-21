//! Integration tests for crypto and transport bug fixes
//!
//! Verifies:
//! - H6: AEAD nonce includes conv for domain separation
//! - L3: BingerTransport returns `NotConnected` when no remote is set
//! - L4: DTLS session limit config (`max_sessions`)

#![cfg(any(feature = "aead", feature = "dtls"))]
#![allow(clippy::module_name_repetitions)]

// ═══════════════════════════════════════════════════════════════════
// TEST 1 (H6): AEAD nonce domain separation
// ═══════════════════════════════════════════════════════════════════
//
// Bug: Nonce was `[counter(8) | 0(4)]` — same key + same counter on
// different connections produced identical nonces.
// Fix: Nonce is `[counter(8) | conv(4)]` — different conv values
// produce different nonces even with the same key and counter.

#[cfg(feature = "aead")]
mod aead_nonce_tests {
    use kcp2_std::crypto::{Aes256GcmCrypto, ChaCha20Poly1305Crypto, KcpCrypto};

    /// Domain separation: same plaintext + key, different conv → nonces
    /// differ in the conv portion while the counter portion stays equal.
    #[test]
    fn test_nonce_contains_conv_for_domain_separation() {
        let key = Aes256GcmCrypto::generate_key().unwrap();
        let crypto = Aes256GcmCrypto::new(&key);
        let crypto2 = Aes256GcmCrypto::new(&key);

        let plaintext = b"test message for nonce verification";

        // Each instance starts at counter=0
        let packet_conv100 = crypto.encrypt(100, plaintext).expect("encrypt conv=100");
        let packet_conv200 = crypto2.encrypt(200, plaintext).expect("encrypt conv=200");

        // Packet layout: [CONV(4) | NONCE(12) | CIPHERTEXT | TAG(16)]
        let nonce1 = &packet_conv100[4..16];
        let nonce2 = &packet_conv200[4..16];

        // First 8 bytes = counter (both should be 0)
        assert_eq!(
            &nonce1[..8],
            &nonce2[..8],
            "counter portion should be identical (both counter=0)",
        );

        // Last 4 bytes = conv
        let conv1 = u32::from_le_bytes(nonce1[8..12].try_into().unwrap());
        let conv2 = u32::from_le_bytes(nonce2[8..12].try_into().unwrap());
        assert_eq!(conv1, 100, "nonce conv portion should be 100");
        assert_eq!(conv2, 200, "nonce conv portion should be 200");

        // Whole nonces must differ (domain separation working)
        assert_ne!(nonce1, nonce2, "nonces must differ for different conv values");
    }

    /// Encrypt/decrypt roundtrip with conv embedded in nonce still works.
    #[test]
    fn test_decrypt_with_correct_conv_nonce() {
        let key = Aes256GcmCrypto::generate_key().unwrap();
        let crypto = Aes256GcmCrypto::new(&key);

        let plaintext = b"decrypt test with conv nonce";
        let packet = crypto.encrypt(42, plaintext).expect("encrypt");
        let decrypted = crypto.decrypt(&packet).expect("decrypt");

        assert_eq!(decrypted, plaintext);
    }

    /// ChaCha20-Poly1305 follows the same nonce construction.
    #[test]
    fn test_chacha20_nonce_contains_conv() {
        let key = ChaCha20Poly1305Crypto::generate_key().unwrap();
        let crypto = ChaCha20Poly1305Crypto::new(&key);
        let crypto2 = ChaCha20Poly1305Crypto::new(&key);

        let plaintext = b"chacha20 domain separation test";

        let p1 = crypto.encrypt(50, plaintext).expect("encrypt conv=50");
        let p2 = crypto2.encrypt(60, plaintext).expect("encrypt conv=60");

        let nonce1 = &p1[4..16];
        let nonce2 = &p2[4..16];

        // Counter portions equal (both counter=0)
        assert_eq!(&nonce1[..8], &nonce2[..8], "counter portion equal");

        // Conv portions differ
        let conv1 = u32::from_le_bytes(nonce1[8..12].try_into().unwrap());
        let conv2 = u32::from_le_bytes(nonce2[8..12].try_into().unwrap());
        assert_eq!(conv1, 50);
        assert_eq!(conv2, 60);

        assert_ne!(nonce1, nonce2, "nonces must differ for different conv");
    }

    /// ChaCha20 roundtrip still works.
    #[test]
    fn test_chacha20_decrypt_roundtrip() {
        let key = ChaCha20Poly1305Crypto::generate_key().unwrap();
        let crypto = ChaCha20Poly1305Crypto::new(&key);

        let plaintext = b"chacha20 roundtrip";
        let packet = crypto.encrypt(7, plaintext).expect("encrypt");
        let decrypted = crypto.decrypt(&packet).expect("decrypt");

        assert_eq!(decrypted, plaintext);
    }
}

// ═══════════════════════════════════════════════════════════════════
// TEST 2 (L3): BingerTransport returns error when no remote is set
// ═══════════════════════════════════════════════════════════════════
//
// Bug: `try_send()` sent to `0.0.0.0:0` when no remote was configured.
// Fix: Returns `Err(io::ErrorKind::NotConnected)`.
//
// SKIPPED: `BingerTransport::new()` takes a `BingerUdp` from the
// `binger-udp` crate (optional dep, not in dev-dependencies).  It is
// not possible to construct a `BingerUdp` from integration tests
// without adding `binger-udp` to `[dev-dependencies]` or having it
// re-exported by `kcp2_std`.
//
// To test manually:
//   kcp2-std/Cargo.toml  →  add `binger-udp` to [dev-dependencies]
//   Then:
//       use binger_udp::BingerUdp;
//       use kcp2_std::transport::BingerTransport;
//       let udp = BingerUdp::bind("127.0.0.1:0").unwrap();
//       let transport = BingerTransport::new(udp);
//       let result = transport.try_send(b"hello");
//       assert!(result.is_err());
//       assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotConnected);
//
// TODO: Revisit when `binger-udp` is added to dev-dependencies or
//       when `BingerTransport` gains a convenience constructor that
//       binds a std `UdpSocket` internally.

// ═══════════════════════════════════════════════════════════════════
// TEST 3 (L4): DTLS session limit config
// ═══════════════════════════════════════════════════════════════════
//
// Bug: No limit on DTLS sessions (resource exhaustion).
// Fix: `DtlsConfig::max_sessions` field with default 1024.

#[cfg(feature = "dtls")]
mod dtls_config_tests {
    use std::time::Duration;
    use kcp2_std::transport::DtlsConfig;

    #[test]
    fn test_dtls_config_max_sessions_default() {
        let config = DtlsConfig::default();
        assert_eq!(
            config.max_sessions, 1024,
            "default max_sessions should be 1024",
        );
    }

    #[test]
    fn test_dtls_config_max_sessions_builder() {
        let config = DtlsConfig::server_psk(b"secret", "kcp2")
            .max_sessions(512);
        assert_eq!(
            config.max_sessions, 512,
            "builder should set max_sessions to 512",
        );
    }

    #[test]
    fn test_dtls_config_max_sessions_custom_default() {
        let psk_cfg = DtlsConfig::client_psk(b"secret", "kcp2");
        assert_eq!(
            psk_cfg.max_sessions, 1024,
            "client PSK config should default to 1024",
        );
    }

    #[test]
    fn test_dtls_config_max_sessions_min_one() {
        // The builder clamps to at least 1
        let config = DtlsConfig::server_psk(b"secret", "kcp2")
            .max_sessions(0);
        assert_eq!(
            config.max_sessions, 1,
            "max_sessions should be clamped to at least 1",
        );
    }

    #[test]
    fn test_dtls_config_preserves_other_fields() {
        let config = DtlsConfig::client_psk(b"secret", "kcp2")
            .handshake_timeout(Duration::from_secs(5))
            .overhead(48)
            .send_queue_size(128)
            .recv_buf_size(2048)
            .max_sessions(256);

        assert_eq!(config.max_sessions, 256);
        assert_eq!(config.handshake_timeout, Duration::from_secs(5));
        assert_eq!(config.overhead, 48);
        assert_eq!(config.send_queue_size, 128);
        assert_eq!(config.recv_buf_size, 2048);
    }
}

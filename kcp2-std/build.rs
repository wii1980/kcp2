/// Compile-time guard: `aead` and `dtls` features are mutually exclusive.
///
/// Both provide encryption; enabling both would cause double-encryption,
/// wasting CPU and bandwidth. Fail early with a clear message.
fn main() {
    const _: () = {
        assert!(
            !(cfg!(feature = "aead") && cfg!(feature = "dtls")),
            r"

  features `aead` and `dtls` are mutually exclusive.

  - `aead`: per-packet AEAD (AES-256-GCM / ChaCha20-Poly1305), 32-byte overhead
  - `dtls`: DTLS 1.2 transport-layer encryption, ~64-byte overhead

  Pick one. See kcp2-std documentation for details.
"
        );
    };
}

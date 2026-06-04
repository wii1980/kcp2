# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Changed

- **Per-connection memory reduced ~82%** (from ~17.4 KB to ~3.1 KB in idle state)
  - `acklist` initial capacity: 256 → 16
  - `Kcp.buffer`: eager `vec![0u8; MTU*3]` → lazy `Vec::new()`, allocated on first `flush()`
  - `OUTPUT_QUEUE_CAPACITY`: 512 → 32
  - `recv_tmp` initial capacity: 2048 → 128
  - `OUTPUT_POOL_CAPACITY`: 64 → 16
  - mpsc channel capacity: 64 → 16 (configurable via `KcpConfig::channel_capacity`)

### Added

- `KcpConfig::channel_capacity(n)` — configure mpsc channel capacity per connection (default 16, min 4)
- `KcpConfig::max_wait_snd(n)` — set backpressure threshold for pending send segments (default 0 = disabled)
- `KcpConfig::pending_send_cap(n)` — configure pending send buffer capacity
- `KcpConnection::send_with_backpressure()` — send with backpressure check, returns `Err(SendBackpressure)` when `wait_snd >= max_wait_snd`
- `KcpError::SendBackpressure` — new error variant for backpressure rejection

### Changed

- Rename dependency `udp-binger` → `binger-udp` (crate rename upstream)
- Move `aead`/`dtls` mutual exclusion check from `build.rs` to `compile_error!` in `lib.rs`
- Remove unused `EmbKcpConfig::update_interval_ms` field from `kcp2-embassy`
- Remove unused `ConnectionReaper::run()` method (use `run_with_cleanup` instead)
- Replace bare `.unwrap()` calls with descriptive `.expect()` messages in protocol core and transport layer
- Extract `u32_from_le`/`u16_from_le` helpers in `segment.rs` to eliminate repeated `try_into().unwrap()` pattern
- Refactor `flush()` macro in `alloc_impl`/`heapless_impl` to eliminate `unused_assignments` lint suppression
- Use idiomatic `.iter().take(n)` in listener batch recv loop

### Benchmarks

Benchmark comparison (main vs feature/memory-optimization):

| Metric | Before | After | Change |
|--------|--------|-------|--------|
| Single connection create | 8.85 µs | 7.77 µs | -12.2% |
| Batch create 1k connections | 13.1 ms | 7.79 ms | -40.5% |
| Batch create 5k connections | 62.2 ms | 44.0 ms | -29.3% |
| KCP send (1000 connections) | 651 µs | 84 µs | -87.1% |
| KCP input (1000 connections) | 972 µs | 116 µs | -88.1% |
| Listener create 1k throughput | 111 Kelem/s | 170 Kelem/s | +53.6% |
| Concurrent DashMap lookup | 107 µs | 96 µs | -10.3% |

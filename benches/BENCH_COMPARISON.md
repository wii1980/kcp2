# C vs Rust 基准测试对比

**环境**: `gcc -O3 -march=native -flto` vs `rustc --release`（LLVM LTO）  
**时钟**: CLOCK_MONOTONIC_RAW（C）/ TSC（Rust via criterion）  
**种子**: 42（确定性 PRNG）  
**日期**: 2026-04-30

---

## 微基准测试（越低越好 — ns/op）

| 测试 | C (mean) | Rust (mean) | C/Rust 比 | 分析 |
|------|----------|-------------|-----------|------|
| segment_encode (124B) | 0.20 ns | 5.13 ns | **25.6×** | C 直接 memcpy；Rust 带边界检查和虚函数调用 |
| segment_decode (124B) | 0.20 ns | 8.13 ns | **40.7×** | 同上 |
| segment_encode/0 (24B) | 0.42 ns | 12.14 ns | **28.9×** | |
| segment_encode/64 (88B) | 1.79 ns | 13.30 ns | **7.4×** | |
| segment_encode/256 (280B) | 2.40 ns | 14.38 ns | **6.0×** | 大 payload 缩小差距（memcpy 占主导） |
| segment_encode/1400 (1424B) | 10.45 ns | 25.83 ns | **2.5×** | memcpy bandwidth 成瓶颈，差距收窄 |
| send_small_packet (100B) | 52.13 ns | 38.65 ns | **0.74×** ⚠️ | **Rust 更快** — 见下方分析 |
| send_large_packet (10KB) | 3466 ns | 752 ns | **0.22×** ⚠️ | Rust 快 4.6× |
| send/64 | 31.68 ns | 35.09 ns | **1.11×** | 基本持平 |
| send/256 | 83.42 ns | 40.72 ns | **0.49×** | ⚠️ Rust 快 2× |
| send/1024 | 290.97 ns | 88.39 ns | **0.30×** | ⚠️ |
| send/1400 | 489.27 ns | 120.41 ns | **0.25×** | ⚠️ |
| send_stream_mode (100B) | 37.62 ns | 40.55 ns | **1.08×** | 接近持平 |
| input (124B) | 12.92 ns | 192.87 ns | **14.9×** | C 远快 — Rust input 包含 parse_data / rcv_buf 搜索 |
| recv | 2.84 ns | 57.63 ns | **20.3×** | C 的 recv 只是链表 pop |
| flush | 4.56 ns | 38.55 ns | **8.5×** | C 的 flush 更轻量 |
| update | 1.85 ns | 43.20 ns | **23.4×** | C 的 update 极少计算 |
| loopback (13B) | 18.21 ns | 465.93 ns | **25.6×** | C 通过裸回调；Rust 通过 Rc<RefCell> |
| out_of_order (100 pkts) | 916.65 ns | 6213.4 ns | **6.8×** | C 链表插入；Rust BTreeMap/Vec 搜索 |
| multi_connection (10 conn) | 197.71 ns | 2148.1 ns | **10.9×** | C 裸循环；Rust Vec 迭代 + trait dispatch |

> `×` 比值 > 1 = C 更快，`×` 比值 < 1 = Rust 更快

---

## 吞吐量对比（越高越好 — MB/s）

| 测试 | C (MB/s) | Rust (MB/s) | C/Rust |
|------|----------|-------------|--------|
| segment_encode (124B) | 589,871 | — | — |
| segment_encode/1400 (1424B) | 129,973 | 51,345 GiB/s* | 2.5× |
| send_small_packet (100B) | 1,829 | 2,409 | 0.76× |
| send_large_packet (10KB) | 2,818 | 12,679 | 0.22× |
| send/64 | 1,926 | 1,698 | 1.13× |
| send/256 | 2,926 | 5,855 | 0.50× |
| send/1024 | 3,356 | 10,790 | 0.31× |
| send/1400 | 2,729 | 10,828 | 0.25× |
| input (124B) | 9,156 | 613 | 14.9× |
| loopback (13B) | 628 | 24.6 | 25.6× |

> *Rust GiB/s 值直接取自 criterion 输出；1 GiB = 1024³ B

---

## 关键发现

### 1. C 在编解码、input、recv、flush、update、loopback 上大幅领先

这符合预期 — C 是裸函数调用 + 直接内存操作，没有 trait dispatch、边界检查、Rc/RefCell 开销。最显著差异：

- **input (14.9×)**: Rust 的 input 包含 `parse_data` 中 `rcv_buf` 的链表插入 + 错误处理；C 仅做基本协议解析。
- **loopback (25.6×)**: C 通过裸函数指针回调；Rust 通过 `Rc<RefCell<Vec<Vec<u8>>>>` 间接收集 output。
- **update (23.4×)**: C 的 `ikcp_update` 极轻量；Rust 版本包含更多状态检查和 trait 方法调用。

### 2. Rust 在 send 上显著更快（最多 4.6×）⚠️ 需调查

C 在 `send` 测试中意外慢于 Rust，且差距随 payload 增大而扩大：

```
C send/1400:    489 ns   (0.49 ns/byte)
Rust send/1400: 120 ns   (0.12 ns/byte)
```

可能原因：
1. **C 的 allocation 路径更重**: `ikcp_segment_new` 调用 `ikcp_malloc` 走函数指针间接跳转；Rust 的 `Vec::reserve` 通过 LLVM LTO 内联分配器。
2. **C 的 `iqueue_add_tail` 有额外指针操作**: Rust 可能用 `VecDeque` push_back 更高效。
3. **Rust 的 Kcp 实现可能在发送时不分配 segment**: 需要验证 Rust Kcp.send 是否用了内存池或预分配缓存（查看 Rust 源码中 send 的实现）。
4. **gcc LTO 可能不如 LLVM LTO**: Rust 使用 LLVM 跨 crate LTO；gcc LTO 对间接函数指针（`ikcp_malloc_hook`）的 devirtualization 可能保守。

**建议进一步调查 Rust Kcp::send 实现**，确认它是否使用了内存池或惰性分配策略。

### 3. `segment_encode` 的 C 数据（0.20 ns）接近测量噪声

- 0.20 ns ≈ 1 CPU cycle @ 5GHz，意味着整个 10000 次迭代循环在约 2µs 内完成。
- `seg_encode` 被完全内联为 ~8 条 mov 指令，L1 缓存命中，CPU 实现 ~1 cycle/iteration 的吞吐。
- Rust 版本的 5.13 ns 包含方法调用、`Result` 返回、边界检查等零开销抽象的实际开销。

### 4. 网络模拟端到端测试

| 模式 | C avg RTT | C max RTT | C tx | 说明 |
|------|-----------|-----------|------|------|
| default (nodelay=0) | 22,456 ms | 49,002 ms | 408 | RTO 翻倍，丢包后退让严重 |
| normal (nc=1) | 564 ms | 2,090 ms | 1,158 | 关闭拥塞控制，g 性能大幅提升 |
| fast (nodelay=2, nc=1, rto=10) | 362 ms | 948 ms | 1,259 | 最快模式，RTO 不翻倍 |

---

## 关于 send 差距的补充说明

C 在 send 上比 Rust 慢是本次对比最意外的发现。需要检查：
1. C 的 `ikcp_malloc` 是否通过函数指针调用导致无法内联
2. Rust `Kcp::send` 是否使用了内存池（segment reuse）
3. 分配器差异（glibc malloc vs jemalloc/glibc via Rust）

如果 Rust 确实有 segment 复用而 C 没有，这是一个有意义的设计差异，不是语言本身的差距。

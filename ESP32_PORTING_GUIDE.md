# rs-kcp2 ESP32 移植方案与测试文档

## 1. 概述

本文档描述如何将 rs-kcp2（Rust 实现的 KCP 协议库）移植到 ESP32 系列设备上运行。KCP 是一个高性能的可靠传输协议，基于 ARQ（自动重传请求）机制，在 WiFi 等不稳定网络环境中提供比 TCP 更低的延迟。

## 2. 项目架构

### 2.1 三层分离设计

| Crate         | 目标平台                  | 异步运行时 | 网络层                      |
|---------------|---------------------------|------------|-----------------------------|
| `kcp2-core`    | no_std / std              | 无（同步） | 无（output 回调）           |
| `kcp2-std`     | std (Linux/macOS/Windows) | Tokio      | tokio::net::UdpSocket       |
| `kcp2-embassy` | no_std (ESP32)            | Embassy    | embassy-net::udp::UdpSocket |

### 2.2 Feature Gate 设计

```
kcp2-core:
  default = ["std"]
  std     = ["alloc"]            # 完整 std 支持
  alloc   = []                   # 仅 alloc（ESP32 + esp-alloc）
  heapless = ["dep:heapless"]    # 无 alloc，固定大小容器（与 alloc 互斥）
  bytes   = ["dep:bytes", "alloc"] # Bytes 类型支持（隐含 alloc）
  fastack_conserve = []          # 快速 ACK 保护模式

  注意：alloc 和 heapless 互斥。heapless_impl 只在 !alloc && heapless 时编译。
  bytes 会自动启用 alloc，不要在 heapless-only 配置中使用 bytes。

kcp2-std:
  default = ["fastack_conserve"]
  fastack_conserve = ["kcp2-core/fastack_conserve"]

kcp2-embassy:
  default = []
  esp32c3 = []                   # ESP32-C3 目标
  esp32s3 = []                   # ESP32-S3 目标
  aead    = ["dep:aes-gcm", "dep:chacha20poly1305"]  # AEAD 整包加密
```

## 3. 核心协议层改动详情

### 3.1 std 依赖替换清单

| 原始依赖                        | 替换方案                                | 影响文件                      |
|---------------------------------|-----------------------------------------|-------------------------------|
| `std::collections::BTreeMap`    | `alloc::collections::BTreeMap`          | kcp/alloc_impl.rs             |
| `std::collections::VecDeque`    | `alloc::collections::VecDeque`          | kcp/alloc_impl.rs             |
| `std::vec::Vec`                 | `alloc::vec::Vec`                       | kcp/alloc_impl.rs, segment.rs |
| `bytes::Bytes`                  | `alloc::vec::Vec<u8>`                   | segment.rs, kcp/alloc_impl.rs |
| `bytes::BytesMut`               | `alloc::vec::Vec<u8>`                   | kcp/alloc_impl.rs             |
| `std::io::Cursor`               | 直接切片写入 (offset tracking)          | kcp/alloc_impl.rs             |
| `std::io::{Read, Write}`        | `encode_to_slice` / `decode_from_slice` | segment.rs                    |
| `thiserror::Error`              | 手动 `impl Display + Error`             | errors.rs                     |
| `std::sync::OnceLock + Instant` | `Clock` trait + `StdClock` 实现         | lib.rs                        |
| `println!` ×2                   | `log::warn!`                            | kcp/common.rs                 |

### 3.2 时钟注入设计

```rust
// kcp2-core 提供的 Clock trait
pub trait Clock {
    fn now_ms(&self) -> u32;
}

// std 环境：自动可用
pub fn current() -> u32 { /* StdClock */ }

// embassy 环境：用户实现
pub struct EmbassyClock;
impl Clock for EmbassyClock {
    fn now_ms(&self) -> u32 {
        embassy_time::Instant::now().as_millis() as u32
    }
}
```

### 3.3 段编解码改造

新增 `encode_to_slice` 方法用于 no_std：

```rust
// 无需 std::io::Write 的纯切片编码
pub fn encode_to_slice(&self, buf: &mut [u8]) -> Result<usize, KcpError> {
    let total = 24 + self.data.len();
    if buf.len() < total {
        return Err(KcpError::BufferTooSmall { required: total, available: buf.len() });
    }
    buf[0..4].copy_from_slice(&self.conv.to_le_bytes());
    buf[4] = self.cmd;
    buf[5] = self.frg;
    buf[6..8].copy_from_slice(&self.wnd.to_le_bytes());
    buf[8..12].copy_from_slice(&self.ts.to_le_bytes());
    buf[12..16].copy_from_slice(&self.sn.to_le_bytes());
    buf[16..20].copy_from_slice(&self.una.to_le_bytes());
    buf[20..24].copy_from_slice(&(self.data.len() as u32).to_le_bytes());
    buf[24..total].copy_from_slice(&self.data);
    Ok(total)
}
```

## 4. ESP32 网络层架构

### 4.1 网络协议栈

```
┌──────────────────────────────────────┐
│            用户应用                    │
├──────────────────────────────────────┤
│     EmbKcpSession                     │
│     (embassy timer + KCP core)        │
│     send() / recv() / step()          │
├──────────────────────────────────────┤
│     kcp2-core                          │
│     Kcp<Output> (纯协议算法)          │
├──────────────────────────────────────┤
│     embassy-net::udp::UdpSocket       │
│     async UDP 收发                    │
├──────────────────────────────────────┤
│     embassy-net (smoltcp)             │
│     IP/UDP 协议栈 + DHCP              │
├──────────────────────────────────────┤
│     esp-wifi / esp-radio              │
│     WiFi 4 STA 模式                   │
├──────────────────────────────────────┤
│     esp-hal                           │
│     ESP32-C3 / ESP32-S3 硬件抽象      │
└──────────────────────────────────────┘
```

### 4.2 EmbKcpSession API

```rust
// 创建会话
let session = EmbKcpSession::new(conv, socket, remote, config);

// 发送数据
session.send(b"hello").unwrap();

// 异步接收（事件循环）
let n = session.recv(&mut buf).await.unwrap();

// 或手动驱动事件循环
loop {
    session.step().await;        // 处理 UDP + KCP 定时器
    if let Ok(n) = session.try_recv(&mut buf) {
        // 处理收到的数据
    }
}
```

### 4.3 EmbKcpConfig 预设

| 预设                     | 场景     | WND     | MTU  | Interval |
|--------------------------|----------|---------|------|----------|
| `default()`              | 通用     | 32/128  | 1400 | 100ms    |
| `high_latency()`         | 卫星链路 | 512/512 | 1400 | 150ms    |
| `high_loss()`            | 无线网络 | 256/256 | 1400 | 80ms     |
| `low_latency()`          | 内网     | 512/512 | 1400 | 10ms     |
| `embedded_constrained()` | 内存受限 | 16/16   | 512  | 50ms     |

## 5. 内存预算

### 5.1 ESP32-C3 (400KB SRAM)

| 组件                       | 占用  | 说明                       |
|----------------------------|-------|----------------------------|
| WiFi 固件 + 驱动           | ~60KB | esp-radio                  |
| esp-alloc 堆               | 72KB  | 用户可用                   |
| embassy-net StackResources | ~16KB | StackResources<3>          |
| UDP socket 缓冲区          | ~8KB  | rx + tx buffer             |
| KCP 单连接                 | ~16KB | snd_buf + rcv_buf + buffer |
| 应用逻辑                   | ~20KB | 用户代码                   |
| 系统/中断栈                | ~16KB |                            |

### 5.2 ESP32-S3 (512KB SRAM + 8MB PSRAM)

| 组件                       | 占用      | 说明              |
|----------------------------|-----------|-------------------|
| WiFi 固件 + 驱动           | ~60KB     | esp-radio         |
| esp-alloc 堆 (SRAM)        | 72KB      | 内部 SRAM         |
| PSRAM 堆                   | 可达数 MB | 大缓冲区放 PSRAM  |
| embassy-net StackResources | ~16KB     | StackResources<3> |
| KCP 连接 (PSRAM)           | ~50KB+    | 可用更大窗口      |

## 6. 构建工具链安装

### 6.1 环境准备

```bash
# 安装 espup（ESP32 Rust 工具链管理器）
cargo install espup
espup install

# 加载环境变量（每次新 shell 需执行）
. $HOME/export-esp.sh

# 安装烧录工具
cargo install espflash
```

### 6.2 编译目标

| 芯片     | 架构   | 编译目标                      | Rust 版本 |
|----------|--------|-------------------------------|-----------|
| ESP32    | Xtensa | `xtensa-esp32-none-elf`       | Nightly   |
| ESP32-S3 | Xtensa | `xtensa-esp32s3-none-elf`     | Nightly   |
| ESP32-C3 | RISC-V | `riscv32imc-unknown-none-elf` | Stable    |

### 6.3 编译命令

```bash
# kcp2-core: std 模式编译检查
cargo check -p kcp2-core --features std

# kcp2-core: alloc 模式编译检查（ESP32 用）
cargo check -p kcp2-core --no-default-features --features alloc

# kcp2-std: 全部测试
cargo test -p kcp2-std

# ESP32-C3 Echo 示例
cd examples/embassy-esp32 && ./build.sh --chip c3

# ESP32-S3 Echo 示例
cd examples/embassy-esp32 && ./build.sh --chip s3

# ESP32-C3 心跳示例
cd examples/embassy-esp32-heartbeat && ./build.sh --chip c3

# ESP32-S3 心跳示例
cd examples/embassy-esp32-heartbeat && ./build.sh --chip s3

# 心跳示例 + AEAD 加密（ChaCha20-Poly1305）
cd examples/embassy-esp32-heartbeat && ./build.sh --chip c3 --aead

# 心跳示例 + AEAD 加密（AES-256-GCM）
cd examples/embassy-esp32-heartbeat && ./build.sh --chip c3 --aes
```

### 6.4 烧录与监控

```bash
# 烧录 + 串口监控（Echo 示例）
cd examples/embassy-esp32 && ./build.sh monitor --chip c3
cd examples/embassy-esp32 && ./build.sh monitor --chip s3

# 烧录 + 串口监控（心跳示例）
cd examples/embassy-esp32-heartbeat && ./build.sh monitor --chip c3
cd examples/embassy-esp32-heartbeat && ./build.sh flash --chip s3

# 烧录 + 串口监控 + AEAD 加密
cd examples/embassy-esp32-heartbeat && ./build.sh monitor --chip c3 --aead
cd examples/embassy-esp32-heartbeat && ./build.sh monitor --chip s3 --aes
```

## 7. 测试方法与流程

### 7.1 单元测试（主机上运行）

```bash
# kcp2-core 协议核心测试
cargo test -p kcp2-core --features std

# kcp2-core alloc 模式测试
cargo test -p kcp2-core --no-default-features --features alloc

# kcp2-std 异步层测试
cargo test -p kcp2-std

# 向后兼容测试
cargo test -p kcp2
```

**测试覆盖项**：

| 测试项                 | 验证内容             | 预期结果                         |
|------------------------|----------------------|----------------------------------|
| segment_roundtrip      | Segment 编解码一致性 | encode → decode 数据相同         |
| kcp_send_recv_echo     | 同步回环通信         | send("hello") → recv() = "hello" |
| kcp_conv_mismatch      | 错误会话 ID          | 返回 ConvMismatch 错误           |
| kcp_window_management  | 窗口滑动正确性       | 按 SN 顺序交付                   |
| kcp_retransmission     | 重传机制             | 丢包后数据仍完整                 |
| kcp_congestion_control | 拥塞控制             | cwnd 正确更新                    |

### 7.2 编译验证

```bash
# 验证 no_std 编译（无需 ESP32 工具链）
cargo check -p kcp2-core --no-default-features --features alloc

# 验证 kcp2-embassy 编译
cargo check -p kcp2-embassy
```

### 7.3 硬件集成测试

#### 准备工作

1. ESP32 开发板（C3 或 S3）
2. WiFi AP（可用手机热点）
3. PC 端安装 Rust 工具链 + kcp2-std

#### 测试步骤

##### 步骤 1：ESP32 端部署

```bash
# 设置 WiFi 凭据（修改 src/main.rs 中的 SSID/PASSWORD 常量）
# 同时修改 SERVER_IP 为你的 PC IP 地址（心跳示例）

# 编译并烧录 Echo 示例
cd examples/embassy-esp32
./build.sh flash --chip c3

# 或编译并烧录心跳示例（需先启动 PC 端 multi_server）
cd examples/embassy-esp32-heartbeat
./build.sh flash --chip c3
```

串口输出应显示：
```
=== ESP32 KCP Echo START ===
Got IP: 192.168.1.xxx
KCP Echo listening on port 8888
```

心跳示例输出：
```
=== ESP32 KCP Heartbeat Client START ===
Got IP: 192.168.1.xxx
[kcp] Session created, conv=0x11223344
[kcp] Connected! Starting heartbeat
[heartbeat] Sending #1
[heartbeat] #1 confirmed: HEARTBEAT_1
```

##### 步骤 2：PC 端测试（使用 kcp2-std 客户端）

```bash
# 运行 multi_server 作为 echo 服务端
cargo run --example multi_server server

# 运行 high_level_api 示例连接 ESP32 Echo
cargo run --example high_level_api client -- 192.168.1.xxx:8888

# 或运行心跳客户端（连接 PC 端 multi_server）
cargo run --example heartbeat
```

##### 步骤 3：功能验证

| 测试项    | 操作                    | 预期结果          |
|-----------|-------------------------|-------------------|
| WiFi 连接 | ESP32 上电              | 串口显示 "Got IP" |
| UDP 收发  | PC 发 UDP 到 ESP32:8888 | ESP32 回显        |
| KCP 握手  | PC 用 KcpConnector 连接 | 连接成功          |
| Echo 测试 | PC 发送 "hello"         | PC 收到 "hello"   |
| 稳定性    | 持续发送 1 小时         | 无 OOM、无死链    |
| 延迟测试  | ping-pong 1000 次       | 平均延迟 < 50ms   |

##### 步骤 4：网络异常测试

| 测试项    | 操作                | 预期结果       |
|-----------|---------------------|----------------|
| WiFi 断连 | 关闭 AP 5 秒后重启  | ESP32 自动重连 |
| 丢包测试  | 用 tc 模拟 10% 丢包 | KCP 仍正常传输 |
| 高延迟    | tc 模拟 200ms 延迟  | KCP 仍正常传输 |

### 7.4 性能基准

```bash
# 主机上运行 KCP benchmark
cargo bench -p kcp2-core
```

预期性能（ESP32-C3, 160MHz）：
- 吞吐量：~500 KB/s（受限于 WiFi 带宽）
- 延迟：< 30ms（LAN 环境）
- 内存占用：< 150KB

## 8. 使用示例

### 8.1 PC 端（std）— 服务端

```rust
use kcp2::{KcpConfig, KcpListener};
use tokio::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = KcpConfig::default()
        .nodelay(true, 10, 2, true)
        .wndsize(512, 512)
        .timeout(Duration::from_secs(30));

    let listener = KcpListener::bind_with_config("0.0.0.0:8888", config).await?;
    loop {
        let (conn, addr) = listener.accept().await?;
        tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            loop {
                match conn.recv(&mut buf).await {
                    Ok(size) if size > 0 => { conn.send(&buf[..size]).await.unwrap(); }
                    Err(_) => break,
                    _ => {}
                }
            }
        });
    }
}
```

### 8.2 ESP32 端（embassy）— 客户端

```rust
use kcp2_embassy::{EmbKcpSession, EmbKcpConfig, EmbassyClock};
use kcp2_core::Clock;

// 创建 KCP 会话连接到 PC
let config = EmbKcpConfig::embedded_constrained();
let session = EmbKcpSession::new(
    1,                              // conv
    udp_socket,                     // embassy-net UdpSocket
    remote_endpoint,                // PC 地址
    config,
);

// 发送数据
session.send(b"hello from ESP32").unwrap();

// 异步接收
let mut buf = [0u8; 1500];
let n = session.recv(&mut buf).await.unwrap();
```

## 9. 依赖版本参考

### kcp2-core

| 依赖       | 版本           | 说明                    |
|------------|----------------|-------------------------|
| `log`      | 0.4            | 日志门面（no_std 兼容） |
| `heapless` | 0.8 (optional) | 无 alloc 固定大小容器   |

### kcp2-std

| 依赖              | 版本 | 说明         |
|-------------------|------|--------------|
| `kcp2-core`        | path | 核心协议     |
| `tokio`           | 1    | 异步运行时   |
| `bytes`           | 1    | 零拷贝缓冲区 |
| `dashmap`         | 6    | 并发 HashMap |
| `crossbeam-queue` | 0.3  | 无锁队列     |
| `parking_lot`     | 0.12 | 高性能 Mutex |

### kcp2-embassy

| 依赖               | 版本 | 说明                     |
|--------------------|------|--------------------------|
| `kcp2-core`         | path | 核心协议 (alloc feature) |
| `embassy-executor` | 0.10 | 异步执行器               |
| `embassy-time`     | 0.5  | 时间原语                 |
| `embassy-net`      | 0.9  | TCP/IP 栈                |
| `embassy-sync`     | 0.8  | 同步原语                 |
| `embassy-futures`  | 0.1  | 异步工具                 |
| `heapless`         | 0.8  | 固定大小容器             |
| `log`              | 0.4  | 日志门面                 |
| `static_cell`      | 2    | 静态内存分配             |

### ESP32 示例

| 依赖               | 版本  | 说明                           |
|--------------------|-------|--------------------------------|
| `esp-hal`          | 1.0.0 | 硬件抽象层                     |
| `esp-alloc`        | 0.9   | 堆分配器                       |
| `esp-backtrace`    | 0.18  | 调试信息                       |
| `esp-println`      | 0.16  | 串口输出                       |
| `esp-rtos`         | 0.2   | RTOS 集成 + embassy + esp-radio |
| `esp-radio`        | 0.17  | WiFi 无线驱动                  |
| `embassy-executor` | 0.9   | 异步执行器                     |
| `embassy-net`      | 0.9   | TCP/IP 栈                      |
| `embassy-futures`  | 0.1   | 异步工具（心跳示例用）         |
| `static_cell`      | 2     | 静态内存分配                   |

## 10. 常见问题

### Q: ESP32-C3 内存不够怎么办？

使用 `EmbKcpConfig::embedded_constrained()` 预设（WND=16, MTU=512），或减小 `esp_alloc::heap_allocator!` 的大小。

### Q: embassy-net 版本不匹配？

esp-rs 生态系统版本更新频繁。如果 `kcp2-embassy` 编译失败，请检查 esp-hal 的示例代码中使用的 embassy 版本，并在 `kcp-embassy/Cargo.toml` 中对齐。

### Q: 如何在 ESP32 上使用 PSRAM？

ESP32-S3 内建 8MB PSRAM。在 main 函数中添加：
```rust
esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);
```
然后 KCP 的大缓冲区会自动分配到 PSRAM。

### Q: 如何调试 ESP32 上的 KCP？

通过 `esp-println` 和 `log` crate 宏（`log::debug!`、`log::warn!`）输出到串口。使用 `espflash flash --monitor` 查看实时日志。

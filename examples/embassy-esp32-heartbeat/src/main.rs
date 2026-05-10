//! ESP32 KCP Heartbeat 客户端示例
//!
//! WiFi 连接 + embassy-net + KCP 心跳通讯，实现与 `heartbeat.rs` 相同的功能：
//! 1. 连接 KCP 服务器（multi_server echo）
//! 2. 定时发送心跳包，等待服务端 echo 确认
//! 3. 连续心跳失败后判定断线
//! 4. 自动重连（指数退避 + 抖动）
//!
//! 先启动服务器: cargo run --example multi_server server
//! 再烧录 ESP32: ./build.sh flash --chip c3
//!
//! 编译:
//! - ESP32-C3: cargo build --release --target riscv32imc-unknown-none-elf --features esp32c3
//! - ESP32-S3: cargo build --release --target xtensa-esp32s3-none-elf --features esp32s3 -Z build-std=core,alloc

#![no_std]
#![no_main]

extern crate alloc;

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_net::{
    udp::{PacketMetadata, UdpSocket},
    Ipv4Address, Runner, StackResources,
};
use embassy_time::{Duration, Instant, Timer};
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{clock::CpuClock, rng::Rng, timer::timg::TimerGroup};
#[cfg(target_arch = "riscv32")]
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_println::println;
use esp_radio::wifi::{
    AuthMethod, ClientConfig, Config, ModeConfig, ScanConfig, WifiController, WifiDevice,
};
use alloc::boxed::Box;
use kcp2_embassy::{EmbKcpConfig, EmbKcpSession};
#[cfg(all(feature = "aead", not(feature = "aead-aes")))]
use kcp2_embassy::crypto::ChaCha20Poly1305Crypto;
#[cfg(feature = "aead-aes")]
use kcp2_embassy::crypto::Aes256GcmCrypto;

esp_bootloader_esp_idf::esp_app_desc!();

// ── WiFi 配置 ──────────────────────────────────────────────
// const SSID: &str = "RestoSuite-Office";
// const PASSWORD: &str = "Fzgj@1801";
const SSID: &str = "wii-test";
const PASSWORD: &str = "123456789";

// ── KCP 服务器配置 ─────────────────────────────────────────
// ⚠️ 修改为你的服务器 IP 地址（与 multi_server 运行的机器一致）
const SERVER_IP: [u8; 4] = [104, 128, 81, 247];
const SERVER_PORT: u16 = 12345;
const CONV: u32 = 0x44332211;
const LOCAL_PORT: u16 = 23456;

// ── AEAD 密钥（与服务器共享的 PSK，32 字节）─────────────────
#[cfg(feature = "aead")]
const AEAD_KEY: [u8; 32] = [
    0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef,
    0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10,
    0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef,
    0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10,
];

// ── 心跳参数 ───────────────────────────────────────────────
const HEARTBEAT_INTERVAL_SECS: u64 = 15;
const MAX_CONSECUTIVE_FAILURES: u32 = 3;
const ACK_TIMEOUT_SECS: u64 = 5;
const INITIAL_BACKOFF_SECS: u64 = 2;
const MAX_BACKOFF_SECS: u64 = 60;

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

fn u32_to_ascii(val: u32, buf: &mut [u8; 11]) -> &[u8] {
    if val == 0 {
        buf[0] = b'0';
        return &buf[..1];
    }
    let mut n = val;
    let mut pos = 0usize;
    while n > 0 {
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
        pos += 1;
    }
    buf[..pos].reverse();
    &buf[..pos]
}

fn write_heartbeat(buf: &mut [u8; 32], counter: u32) -> usize {
    let prefix = b"HEARTBEAT_";
    buf[..prefix.len()].copy_from_slice(prefix);
    let mut num_buf = [0u8; 11];
    let num_str = u32_to_ascii(counter, &mut num_buf);
    buf[prefix.len()..prefix.len() + num_str.len()].copy_from_slice(num_str);
    prefix.len() + num_str.len()
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    println!("=== ESP32 KCP Heartbeat Client START ===");
    #[cfg(target_has_atomic = "ptr")]
    log::set_max_level(log::LevelFilter::Off);
    esp_println::logger::init_logger(log::LevelFilter::Warn);
    #[cfg(target_has_atomic = "ptr")]
    log::set_max_level(log::LevelFilter::Warn);

    // 使用最低频率 80MHz 以降低功耗和发热（WiFi 最低要求即为 80MHz）
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::_80MHz);
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(size: 72 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    #[cfg(target_arch = "riscv32")]
    {
        let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
        esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);
    }
    #[cfg(target_arch = "xtensa")]
    {
        esp_rtos::start(timg0.timer0);
    }

    let esp_radio_controller = mk_static!(esp_radio::Controller, esp_radio::init().unwrap());

    let (mut controller, interfaces) =
        esp_radio::wifi::new(esp_radio_controller, peripherals.WIFI, Config::default()).unwrap();

    controller
        .set_config(&ModeConfig::Client(
            ClientConfig::default()
                .with_ssid(SSID.into())
                .with_auth_method(AuthMethod::WpaWpa2Personal)
                .with_password(PASSWORD.into()),
        ))
        .unwrap();
    println!("[wifi] Config set (SSID: {})", SSID);

    controller.start_async().await.unwrap();

    let scan_config = ScanConfig::default().with_ssid(SSID);
    let aps = controller
        .scan_with_config_async(scan_config)
        .await
        .unwrap();
    if aps.is_empty() {
        println!("[wifi] SSID '{}' not found!", SSID);
    } else {
        for ap in &aps {
            println!(
                "[wifi] AP: {} rssi:{} ch:{:?}",
                ap.ssid, ap.signal_strength, ap.channel
            );
        }
    }

    let best = aps.into_iter().max_by_key(|ap| ap.signal_strength);
    let client_config = match best {
        Some(ap) => {
            let auth = ap.auth_method.unwrap_or(AuthMethod::WpaWpa2Personal);
            ClientConfig::default()
                .with_ssid(ap.ssid.clone())
                .with_bssid(ap.bssid)
                .with_auth_method(auth)
                .with_password(PASSWORD.into())
                .with_channel(ap.channel)
        }
        None => ClientConfig::default()
            .with_ssid(SSID.into())
            .with_auth_method(AuthMethod::WpaWpa2Personal)
            .with_password(PASSWORD.into()),
    };
    controller.set_config(&ModeConfig::Client(client_config)).unwrap();

    let wifi_interface = interfaces.sta;

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    let net_config = embassy_net::Config::dhcpv4(Default::default());
    let (stack, runner) = embassy_net::new(
        wifi_interface,
        net_config,
        mk_static!(StackResources<3>, StackResources::<3>::new()),
        seed,
    );

    spawner.must_spawn(connection(controller));
    spawner.must_spawn(net_task(runner));
    println!("[net] Waiting for DHCP...");

    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        println!("[net] Got IP: {}", cfg.address);
    }

    // ── KCP Heartbeat 客户端主循环 ──────────────────────────
    let mut heartbeat_counter: u32 = 0;
    let mut backoff_secs: u64 = INITIAL_BACKOFF_SECS;
    let mut jitter_counter: u32 = 0;
    let mut msg_buf = [0u8; 32];
    let mut recv_buf = [0u8; 512];

    let remote = embassy_net::IpEndpoint {
        addr: embassy_net::IpAddress::Ipv4(Ipv4Address::from(SERVER_IP)),
        port: SERVER_PORT,
    };

    loop {
        // ── 创建 KCP 会话 ──────────────────────────────
        let mut rx_meta = [PacketMetadata::EMPTY; 8];
        let mut rx_buffer = [0u8; 2048];
        let mut tx_meta = [PacketMetadata::EMPTY; 8];
        let mut tx_buffer = [0u8; 2048];
        let mut socket = UdpSocket::new(
            stack,
            &mut rx_meta,
            &mut rx_buffer,
            &mut tx_meta,
            &mut tx_buffer,
        );
        socket.bind(LOCAL_PORT).ok();

        let mut session = EmbKcpSession::new_with_crypto(
            CONV, socket, remote, EmbKcpConfig::embedded_constrained(), make_crypto(),
        );
        println!("[kcp] Session created, conv={:#x}, target={}:{}{}", 
            CONV, Ipv4Address::from(SERVER_IP), SERVER_PORT,
            if cfg!(feature = "aead-aes") { " (AEAD AES-256-GCM)" }
            else if cfg!(feature = "aead") { " (AEAD ChaCha20-Poly1305)" }
            else { "" }
        );

        // ── 发送 HELLO ────────────────────────────────
        match session.send_and_flush(b"HELLO").await {
            Ok(_) => println!("[kcp] HELLO sent"),
            Err(e) => {
                println!("[kcp] HELLO send failed: {:?}", e);
                let delay = backoff_secs + simple_jitter(&mut jitter_counter);
                println!("[kcp] Retry in {}s...", delay);
                Timer::after(Duration::from_secs(delay)).await;
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                continue;
            }
        }

        match select(
            session.recv(&mut recv_buf),
            Timer::after(Duration::from_secs(ACK_TIMEOUT_SECS)),
        )
        .await
        {
            Either::First(Ok(n)) => {
                let msg = core::str::from_utf8(&recv_buf[..n]).unwrap_or("?");
                println!("[kcp] Server response: {}", msg);
            }
            Either::First(Err(e)) => {
                println!("[kcp] HELLO recv error: {:?}", e);
                let delay = backoff_secs + simple_jitter(&mut jitter_counter);
                Timer::after(Duration::from_secs(delay)).await;
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                continue;
            }
            Either::Second(()) => {
                println!("[kcp] HELLO timeout");
                let delay = backoff_secs + simple_jitter(&mut jitter_counter);
                Timer::after(Duration::from_secs(delay)).await;
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                continue;
            }
        }

        println!("[kcp] Connected! Starting heartbeat");
        backoff_secs = INITIAL_BACKOFF_SECS;
        let mut consecutive_failures: u32 = 0;

        // ── 心跳循环 ──────────────────────────────────
        let disconnect_reason: &str = loop {
            heartbeat_counter += 1;
            let len = write_heartbeat(&mut msg_buf, heartbeat_counter);
            println!("[heartbeat] Sending #{}", heartbeat_counter);

            if let Err(e) = session.send_and_flush(&msg_buf[..len]).await {
                println!("[heartbeat] Send failed: {:?}", e);
                break "send_failed";
            }

            match select(
                session.recv(&mut recv_buf),
                Timer::after(Duration::from_secs(ACK_TIMEOUT_SECS)),
            )
            .await
            {
                Either::First(Ok(n)) => {
                    let msg = core::str::from_utf8(&recv_buf[..n]).unwrap_or("?");
                    println!("[heartbeat] #{} confirmed: {}", heartbeat_counter, msg);
                    consecutive_failures = 0;
                }
                Either::First(Err(e)) => {
                    println!("[heartbeat] Recv error: {:?}", e);
                    break "recv_error";
                }
                Either::Second(()) => {
                    consecutive_failures += 1;
                    println!(
                        "[heartbeat] #{} timeout ({}/{})",
                        heartbeat_counter,
                        consecutive_failures,
                        MAX_CONSECUTIVE_FAILURES
                    );
                    if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        break "max_failures";
                    }
                }
            }

            if session.is_dead() {
                break "dead_link";
            }

            // 空闲期：接收服务端消息 + 驱动 KCP update
            let deadline = Instant::now() + Duration::from_secs(HEARTBEAT_INTERVAL_SECS);
            loop {
                match select(session.recv(&mut recv_buf), Timer::at(deadline)).await {
                    Either::First(Ok(n)) => {
                        let msg = core::str::from_utf8(&recv_buf[..n]).unwrap_or("?");
                        println!("[heartbeat] Message: {}", msg);
                        continue;
                    }
                    _ => break,
                }
            }
        };

        // ── 断线重连 ──────────────────────────────────
        println!("[kcp] Disconnected: {}", disconnect_reason);
        let jitter = simple_jitter(&mut jitter_counter);
        let delay = backoff_secs + jitter;
        println!("[kcp] Reconnect in {}s...", delay);
        Timer::after(Duration::from_secs(delay)).await;
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
    }
}

#[cfg(all(feature = "aead", not(feature = "aead-aes")))]
fn make_crypto() -> Option<Box<dyn kcp2_embassy::crypto::EmbKcpCrypto>> {
    Some(Box::new(ChaCha20Poly1305Crypto::new(&AEAD_KEY)))
}

#[cfg(feature = "aead-aes")]
fn make_crypto() -> Option<Box<dyn kcp2_embassy::crypto::EmbKcpCrypto>> {
    Some(Box::new(Aes256GcmCrypto::new(&AEAD_KEY)))
}

#[cfg(not(feature = "aead"))]
fn make_crypto() -> Option<Box<dyn kcp2_embassy::crypto::EmbKcpCrypto>> {
    None
}

fn simple_jitter(counter: &mut u32) -> u64 {
    *counter = counter.wrapping_add(1);
    (*counter % 7 + 1) as u64
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    println!("[wifi-task] Connection task started");
    loop {
        match controller.connect_async().await {
            Ok(()) => {
                println!("[wifi-task] Connected!");
                while controller.is_connected().unwrap_or(false) {
                    Timer::after(Duration::from_secs(1)).await;
                }
                println!("[wifi-task] Disconnected");
            }
            Err(e) => {
                println!("[wifi-task] Connect failed: {:?}", e);
            }
        }
        Timer::after(Duration::from_secs(5)).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}

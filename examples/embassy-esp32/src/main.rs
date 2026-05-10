//! ESP32 KCP Echo 示例
//!
//! WiFi 连接 + embassy-net + KCP 协议 Echo 服务器
//!
//! # 编译
//! - ESP32-C3: `cargo build --release --target riscv32imc-unknown-none-elf --features esp32c3`
//! - ESP32-S3: `cargo build --release --target xtensa-esp32s3-none-elf --features esp32s3`
//!
//! # 烧录
//! - ESP32-C3: `espflash flash --monitor target/riscv32imc-unknown-none-elf/release/embassy-esp32-example`
//! - ESP32-S3: `espflash flash --monitor --chip esp32s3 target/xtensa-esp32s3-none-elf/release/embassy-esp32-example`

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_net::{Runner, StackResources, udp::{PacketMetadata, UdpSocket}};
use embassy_time::{Duration, Timer};
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    rng::Rng,
    timer::timg::TimerGroup,
};
#[cfg(target_arch = "riscv32")]
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_println::println;
use esp_radio::wifi::{
    AuthMethod, ClientConfig, Config, ModeConfig, ScanConfig, WifiController, WifiDevice,
};

esp_bootloader_esp_idf::esp_app_desc!();

const SSID: &str = "RestoSuite-Office";
const PASSWORD: &str = "Fzgj@1801";
const KCP_PORT: u16 = 8888;

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    println!("=== ESP32 KCP Echo START ===");
    #[cfg(target_has_atomic = "ptr")]
    log::set_max_level(log::LevelFilter::Off);
    esp_println::logger::init_logger(log::LevelFilter::Warn);
    #[cfg(target_has_atomic = "ptr")]
    log::set_max_level(log::LevelFilter::Warn);
    println!("[1] logger init done (level=Warn)");

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    println!("[2] hal init done");

    esp_alloc::heap_allocator!(size: 72 * 1024);
    println!("[3] heap allocator done (72KB)");

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
    println!("[4] rtos started");

    let esp_radio_controller = mk_static!(esp_radio::Controller, esp_radio::init().unwrap());
    println!("[5] radio init done");

    let (mut controller, interfaces) = esp_radio::wifi::new(
        esp_radio_controller,
        peripherals.WIFI,
        Config::default(),
    ).unwrap();
    println!("[6] wifi new done");

    controller.set_config(&ModeConfig::Client(
        ClientConfig::default()
            .with_ssid(SSID.into())
            .with_auth_method(AuthMethod::WpaWpa2Personal)
            .with_password(PASSWORD.into()),
    )).unwrap();
    println!("[7] wifi config set (SSID: {})", SSID);

    controller.start_async().await.unwrap();
    println!("[7.5] wifi started, scanning for '{}'...", SSID);

    let scan_config = ScanConfig::default().with_ssid(SSID);
    let aps = controller.scan_with_config_async(scan_config).await.unwrap();
    if aps.is_empty() {
        println!("[7.6] ERROR: SSID '{}' not found!", SSID);
    } else {
        for ap in &aps {
            println!(
                "[7.6] found AP: {} | auth: {:?} | rssi: {} | ch: {:?}",
                ap.ssid, ap.auth_method, ap.signal_strength, ap.channel
            );
        }
    }

    let best = aps.into_iter().max_by_key(|ap| ap.signal_strength);
    let client_config = match best {
        Some(ap) => {
            let auth = ap.auth_method.unwrap_or(AuthMethod::WpaWpa2Personal);
            println!("[7.7] connecting to '{}' auth={:?}", ap.ssid, auth);
            ClientConfig::default()
                .with_ssid(ap.ssid.clone())
                .with_bssid(ap.bssid)
                .with_auth_method(auth)
                .with_password(PASSWORD.into())
                .with_channel(ap.channel)
        }
        None => {
            println!("[7.7] fallback: connecting with configured SSID");
            ClientConfig::default()
                .with_ssid(SSID.into())
                .with_auth_method(AuthMethod::WpaWpa2Personal)
                .with_password(PASSWORD.into())
        }
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
    println!("[8] embassy-net stack created");

    spawner.must_spawn(connection(controller));
    spawner.must_spawn(net_task(runner));
    println!("[9] tasks spawned, waiting for network...");

    stack.wait_config_up().await;

    if let Some(config) = stack.config_v4() {
        println!("[10] Got IP: {}", config.address);
    } else {
        println!("[10] WARNING: got config_up but no IPv4!");
    }

    let mut rx_meta = [PacketMetadata::EMPTY; 16];
    let mut rx_buffer = [0u8; 4096];
    let mut tx_meta = [PacketMetadata::EMPTY; 16];
    let mut tx_buffer = [0u8; 4096];

    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buffer, &mut tx_meta, &mut tx_buffer);
    socket.bind(KCP_PORT).unwrap();

    println!("KCP Echo listening on port {}", KCP_PORT);

    let mut recv_buf = [0u8; 1500];
    loop {
        match socket.recv_from(&mut recv_buf).await {
            Ok((n, meta)) => {
                socket.send_to(&recv_buf[..n], meta.endpoint).await.ok();
            }
            Err(_) => {}
        }
    }
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    println!("[conn] WiFi connection task started");
    loop {
        println!("[conn] attempting WiFi connect...");
        match controller.connect_async().await {
            Ok(()) => {
                println!("[conn] WiFi connected!");
                while controller.is_connected().unwrap_or(false) {
                    Timer::after(Duration::from_secs(1)).await;
                }
                println!("[conn] WiFi disconnected");
            }
            Err(e) => {
                println!("[conn] WiFi connect failed: {:?}", e);
            }
        }
        println!("[conn] retrying in 5s...");
        Timer::after(Duration::from_secs(5)).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    println!("[net] network task started");
    runner.run().await
}

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Instant;

use kcp2_core::Kcp;
use once_cell::sync::Lazy;
use rand::Rng;

// ---------------------------------------------------------------------------
// 网络模拟器 — 对应 C bench.c 的 net_sim
// ---------------------------------------------------------------------------
struct DelayPacket {
    data: Vec<u8>,
    ts: u32,
}

struct LatencySimulator {
    lostrate: u32,
    rtt_min: u32,
    rtt_max: u32,
    tx1: u32,
    p12: VecDeque<DelayPacket>,
    p21: VecDeque<DelayPacket>,
}

impl LatencySimulator {
    fn new(lostrate: u32, rtt_min: u32, rtt_max: u32) -> Self {
        Self {
            lostrate: lostrate / 2,
            rtt_min: rtt_min / 2,
            rtt_max: rtt_max / 2,
            tx1: 0,
            p12: VecDeque::new(),
            p21: VecDeque::new(),
        }
    }

    fn send(&mut self, peer: u32, data: &[u8], current: u32) {
        if peer == 0 {
            self.tx1 += 1;
        }
        let mut rng = rand::thread_rng();
        if rng.gen_range(0..1000) < self.lostrate {
            return;
        }
        let delay = if self.rtt_max > self.rtt_min {
            self.rtt_min + rng.gen_range(0..(self.rtt_max - self.rtt_min))
        } else {
            self.rtt_min
        };
        let pkt = DelayPacket {
            data: data.to_vec(),
            ts: current + delay,
        };
        match peer {
            0 => self.p12.push_back(pkt),
            1 => self.p21.push_back(pkt),
            _ => unreachable!(),
        }
    }

    fn recv(&mut self, peer: u32, current: u32) -> Option<Vec<u8>> {
        let queue = if peer == 0 { &mut self.p21 } else { &mut self.p12 };
        queue.make_contiguous().sort_by_key(|a| a.ts);
        if let Some(pkt) = queue.front() {
            if current >= pkt.ts {
                return Some(queue.pop_front().unwrap().data);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// 时钟
// ---------------------------------------------------------------------------
#[allow(clippy::cast_possible_truncation)]
fn now_ms() -> u32 {
    static START: Lazy<Instant> = Lazy::new(Instant::now);
    START.elapsed().as_millis() as u32
}

// ---------------------------------------------------------------------------
// 网络模拟测试
// ---------------------------------------------------------------------------
fn test(mode: u32) {
    let mode_names = ["default", "normal", "fast"];
    println!("\n=== 网络模拟 (模式 {}: {}) ===", mode, mode_names[mode as usize]);

    let sim = Rc::new(RefCell::new(LatencySimulator::new(10, 60, 125)));

    let sim1 = sim.clone();
    let output1 = move |data: &[u8]| {
        sim1.borrow_mut().send(0, data, now_ms());
    };
    let sim2 = sim.clone();
    let output2 = move |data: &[u8]| {
        sim2.borrow_mut().send(1, data, now_ms());
    };

    let mut kcp1 = Kcp::new(0x1122_3344, output1);
    let mut kcp2 = Kcp::new(0x1122_3344, output2);

    kcp1.set_wndsize(128, 128);
    kcp2.set_wndsize(128, 128);

    match mode {
        0 => {
            kcp1.set_nodelay(false, 10, 0, false);
            kcp2.set_nodelay(false, 10, 0, false);
        }
        1 => {
            kcp1.set_nodelay(false, 10, 0, true);
            kcp2.set_nodelay(false, 10, 0, true);
        }
        2 => {
            kcp1.set_nodelay(true, 10, 2, true);
            kcp2.set_nodelay(true, 10, 2, true);
            kcp1.set_rx_minrto(10);
            kcp2.set_rx_minrto(10);
        }
        _ => unreachable!(),
    }

    let mut slap = now_ms() + 20;
    let mut index: u32 = 0;
    let mut next: u32 = 0;
    let mut sum_rtt: u64 = 0;
    let mut max_rtt: u32 = 0;
    let mut count: u32 = 0;

    let start = Instant::now();

    loop {
        std::thread::sleep(std::time::Duration::from_millis(1));
        let current = now_ms();

        kcp1.update(current);
        kcp2.update(current);

        if current >= slap {
            slap += 20;
            let mut buf = [0u8; 8];
            buf[0..4].copy_from_slice(&index.to_le_bytes());
            buf[4..8].copy_from_slice(&current.to_le_bytes());
            index += 1;
            let _ = kcp1.send(&buf);
        }

        while let Some(pkt) = sim.borrow_mut().recv(1, current) {
            let _ = kcp2.input(&pkt);
        }
        while let Some(pkt) = sim.borrow_mut().recv(0, current) {
            let _ = kcp1.input(&pkt);
        }

        loop {
            let mut buf2 = vec![0u8; 10];
            match kcp2.recv(&mut buf2) {
                Ok(n) if n > 0 => { let _ = kcp2.send(&buf2[..n]); }
                _ => break,
            }
        }

        loop {
            let mut buf = vec![0u8; 10];
            match kcp1.recv(&mut buf) {
                Ok(n) if n >= 8 => {
                    let sn = u32::from_le_bytes(buf[0..4].try_into().unwrap());
                    let ts = u32::from_le_bytes(buf[4..8].try_into().unwrap());
                    let rtt = current.wrapping_sub(ts);
                    if sn != next {
                        println!("ERROR sn mismatch: count={count}, next={next}, actual_sn={sn}, rtt={rtt}, ts={ts}, current={current}");
                        println!("  buf bytes: {:02x?}", &buf[..n]);
                        return;
                    }
                    next += 1;
                    sum_rtt += u64::from(rtt);
                    count += 1;
                    if rtt > max_rtt {
                        max_rtt = rtt;
                    }
                    if count <= 5 || count % 100 == 0 {
                        println!("[RECV] mode={mode} sn={sn} rtt={rtt}");
                    }
                }
                _ => break,
            }
        }

        if next > 1000 {
            break;
        }
    }

    let total_ms = start.elapsed().as_millis();
    let avg_rtt = if count > 0 { sum_rtt / u64::from(count) } else { 0 };
    let tx1 = sim.borrow().tx1;

    println!("\n{} mode result ({}ms):", mode_names[mode as usize], total_ms);
    println!("avgrtt={avg_rtt} maxrtt={max_rtt} tx={tx1}");
}

// ---------------------------------------------------------------------------
// 主函数
// ---------------------------------------------------------------------------
fn main() {
    println!("============================================");
    println!("  Rust (kcp2) 网络模拟性能测试");
    println!("============================================");
    println!("网络: 10% 丢包, RTT 60~125ms, 8 字节包 @ 20ms 间隔");

    test(0);
    test(1);
    test(2);

    println!("\n============================================");
    println!("  所有测试完成!");
    println!("============================================");
}

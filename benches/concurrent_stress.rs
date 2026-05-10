//! 高并发压测基准
//!
//! 此文件包含以下场景的高并发压测：
//!
//! - **`high_concurrency_send`**: 大量 KCP 实例并行 send（1k/5k/10k 连接）
//! - **`high_concurrency_loopback`**: 大量 KCP 回环对并行收发（1k/5k 连接）
//! - **`high_concurrency_input`**: 大量 KCP 实例并行 input 数据包
//! - **`high_concurrency_mixed`**: 混合操作（send + update + flush + input + recv）
//! - **`high_concurrency_listener`**: `KcpListener` 在高连接数下的 create/get/remove 操作
//! - **`high_concurrency_tokio_spawn`**: 模拟 tokio 多任务下的 `KcpConnection` send 并发
//!
//! 使用 `cargo bench --bench concurrent_stress` 运行。

use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use kcp2_std::KcpListener;
use rand::Rng;
use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;

use kcp2_core::Kcp;

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

fn rt() -> &'static Runtime {
    static RT: once_cell::sync::OnceCell<Runtime> = once_cell::sync::OnceCell::new();
    RT.get_or_init(|| Runtime::new().unwrap())
}

fn find_free_addr() -> String {
    let sock = StdUdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = sock.local_addr().unwrap();
    drop(sock);
    format!("127.0.0.1:{}", addr.port())
}

#[allow(clippy::cast_possible_truncation)]
fn create_kcp_batch(count: usize) -> Vec<Kcp<impl Fn(&[u8])>> {
    (0..count)
        .map(|i| {
            let mut kcp = Kcp::new(i as u32, |_: &[u8]| {});
            kcp.set_wndsize(64, 64);
            kcp.set_nodelay(true, 10, 2, true);
            kcp
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 高并发 send：大规模 KCP 实例并行 send
// ---------------------------------------------------------------------------

fn bench_high_concurrency_send(c: &mut Criterion) {
    let mut group = c.benchmark_group("high_concurrency_send");

    for &num_conns in &[100, 1000, 5000, 10_000] {
        let data = vec![0u8; 64];

        group.throughput(Throughput::Bytes((num_conns as u64) * 64));
        group.bench_with_input(
            BenchmarkId::new("send", num_conns),
            &num_conns,
            |b, &n| {
                b.iter_batched(
                    || create_kcp_batch(n),
                    |mut kcps| {
                        for kcp in &mut kcps {
                            kcp.send(black_box(&data)).unwrap();
                        }
                        black_box(kcps);
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 高并发 loopback：大规模 KCP 回环对并行收发
// ---------------------------------------------------------------------------

#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn bench_high_concurrency_loopback(c: &mut Criterion) {
    use std::cell::RefCell;
    use std::rc::Rc;

    let mut group = c.benchmark_group("high_concurrency_loopback");

    for &num_pairs in &[50, 200, 500] {
        let msg = b"hello";

        group.throughput(Throughput::Bytes((num_pairs as u64) * (msg.len() as u64)));
        group.bench_with_input(
            BenchmarkId::new("loopback", num_pairs),
            &num_pairs,
            |b, &n| {
                b.iter_batched(
                    || {
                        let mut pairs = Vec::with_capacity(n);
                        for i in 0..n {
                            let buf1: Rc<RefCell<Vec<Vec<u8>>>> =
                                Rc::new(RefCell::new(Vec::new()));
                            let buf2: Rc<RefCell<Vec<Vec<u8>>>> =
                                Rc::new(RefCell::new(Vec::new()));

                            let conv = i as u32;
                            let buf2_c = buf2.clone();
                            let kcp1 = Kcp::new(conv, move |data: &[u8]| {
                                buf2_c.borrow_mut().push(data.to_vec());
                            });

                            let buf1_c = buf1.clone();
                            let kcp2 = Kcp::new(conv, move |data: &[u8]| {
                                buf1_c.borrow_mut().push(data.to_vec());
                            });

                            pairs.push((kcp1, kcp2, buf1, buf2));
                        }
                        pairs
                    },
                    |mut pairs| {
                        for (kcp1, kcp2, _buf1, buf2) in &mut pairs {
                            kcp1.send(msg).unwrap();
                            kcp1.update(0);
                            kcp1.flush();
                            for pkt in buf2.borrow_mut().drain(..) {
                                kcp2.input(&pkt).unwrap();
                            }
                            let mut recv_buf = [0u8; 1024];
                            let _ = kcp2.recv(&mut recv_buf);
                        }
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 高并发 input：大规模 KCP 实例并行 input 数据包
// ---------------------------------------------------------------------------

fn bench_high_concurrency_input(c: &mut Criterion) {
    use kcp2_core::Segment;

    let mut group = c.benchmark_group("high_concurrency_input");

    let mut seg = Segment::new();
    seg.conv = 0;
    seg.cmd = 81;
    seg.frg = 0;
    seg.wnd = 128;
    seg.ts = 1000;
    seg.sn = 1;
    seg.una = 0;
    seg.data = vec![0u8; 50];
    let mut buffer = [0u8; 200];
    let used = seg.encode_to_slice(&mut buffer).unwrap();
    let encoded = buffer[..used].to_vec();

    // All KCP instances must use conv=0 for the pre-encoded packet
    for &num_conns in &[100, 1000, 5000] {
        group.bench_with_input(
            BenchmarkId::new("input", num_conns),
            &num_conns,
            |b, &n| {
                b.iter_batched(
                    || {
                        (0..n)
                            .map(|_| {
                                let mut kcp = Kcp::new(0, |_: &[u8]| {});
                                kcp.set_wndsize(64, 64);
                                kcp.set_nodelay(true, 10, 2, true);
                                kcp
                            })
                            .collect::<Vec<_>>()
                    },
                    |mut kcps| {
                        for kcp in &mut kcps {
                            kcp.input(black_box(&encoded)).unwrap();
                        }
                        black_box(kcps);
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 高并发 mixed：混合操作 — send + update + flush + input + recv
// ---------------------------------------------------------------------------

#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn bench_high_concurrency_mixed(c: &mut Criterion) {
    use std::cell::RefCell;
    use std::rc::Rc;

    let mut group = c.benchmark_group("high_concurrency_mixed");

    for &num_conns in &[50, 200, 500] {
        let data = vec![0u8; 64];

        group.throughput(Throughput::Elements(num_conns as u64));
        group.bench_with_input(
            BenchmarkId::new("mixed", num_conns),
            &num_conns,
            |b, &n| {
                b.iter_batched(
                    || {
                        let mut kcps = Vec::with_capacity(n);
                        let mut outputs = Vec::with_capacity(n);
                        for i in 0..n {
                            let out: Rc<RefCell<Vec<Vec<u8>>>> =
                                Rc::new(RefCell::new(Vec::new()));
                            let out_c = out.clone();
                            let mut kcp = Kcp::new(i as u32, move |d: &[u8]| {
                                out_c.borrow_mut().push(d.to_vec());
                            });
                            kcp.set_wndsize(64, 64);
                            kcp.set_nodelay(true, 10, 2, true);
                            kcps.push(kcp);
                            outputs.push(out);
                        }
                        (kcps, outputs, data.clone())
                    },
                    |(mut kcps, mut outputs, data)| {
                        for kcp in &mut kcps {
                            kcp.send(&data).unwrap();
                            kcp.update(0);
                            kcp.flush();
                        }
                        for (kcp, out) in kcps.iter_mut().zip(outputs.iter_mut()) {
                            for pkt in out.borrow_mut().drain(..) {
                                kcp.input(&pkt).unwrap();
                            }
                            let mut buf = [0u8; 1024];
                            let _ = kcp.recv(&mut buf);
                        }
                        black_box(());
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 高并发 listener：KcpListener 在高连接数下的操作压测
// ---------------------------------------------------------------------------

#[allow(clippy::cast_sign_loss)]
fn bench_high_concurrency_listener_create(c: &mut Criterion) {
    let runtime = rt();

    let mut group = c.benchmark_group("high_concurrency_listener");
    for &num_conns in &[100, 1000, 5000] {
        group.throughput(Throughput::Elements(num_conns as u64));
        group.bench_with_input(
            BenchmarkId::new("create", num_conns),
            &num_conns,
            |b, &n| {
                b.iter_batched(
                    || {
                        let addr = find_free_addr();
                        runtime.block_on(async { KcpListener::bind(&addr).await.unwrap() })
                    },
                    |listener| {
                        let _guard = runtime.enter();
                        let peer: SocketAddr = "127.0.0.1:30000".parse().unwrap();
                        for i in 0..n as u32 {
                            listener.create_connection(i, peer);
                        }
                        black_box(listener.connection_count());
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

#[allow(clippy::cast_sign_loss, clippy::similar_names)]
fn bench_high_concurrency_listener_get(c: &mut Criterion) {
    let runtime = rt();

    let mut group = c.benchmark_group("high_concurrency_listener");
    for &num_conns in &[100, 1000, 5000] {
        group.bench_with_input(
            BenchmarkId::new("get", num_conns),
            &num_conns,
            |b, &n| {
                let addr = find_free_addr();
                let listener = runtime.block_on(async {
                    let listener = KcpListener::bind(&addr).await.unwrap();
                    let _guard = runtime.enter();
                    let peer: SocketAddr = "127.0.0.1:30000".parse().unwrap();
                    for i in 0..n as u32 {
                        listener.create_connection(i, peer);
                    }
                    listener
                });

                let mut rng = rand::thread_rng();
                b.iter(|| {
                    let conv = rng.gen_range(0..n as u32);
                    let conn = listener.get_connection(conv);
                    black_box(conn);
                });
            },
        );
    }
    group.finish();
}

#[allow(clippy::cast_sign_loss)]
fn bench_high_concurrency_listener_remove(c: &mut Criterion) {
    let runtime = rt();

    let mut group = c.benchmark_group("high_concurrency_listener");
    for &num_conns in &[100, 1000] {
        group.throughput(Throughput::Elements(num_conns as u64));
        group.bench_with_input(
            BenchmarkId::new("remove", num_conns),
            &num_conns,
            |b, &n| {
                b.iter_batched(
                    || {
                        let addr = find_free_addr();
                        runtime.block_on(async {
                            let listener = KcpListener::bind(&addr).await.unwrap();
                            let _guard = runtime.enter();
                            let peer: SocketAddr = "127.0.0.1:30000".parse().unwrap();
                            for i in 0..n as u32 {
                                listener.create_connection(i, peer);
                            }
                            listener
                        })
                    },
                    |listener| {
                        for i in 0..n as u32 {
                            listener.remove_connection(i);
                        }
                        black_box(listener.connection_count());
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 高并发 tokio spawn：模拟多任务下的 KcpConnection send
// ---------------------------------------------------------------------------

#[allow(clippy::cast_sign_loss)]
fn bench_high_concurrency_tokio_send(c: &mut Criterion) {
    let runtime = rt();

    let mut group = c.benchmark_group("high_concurrency_tokio");

    for &(num_conns, sends_per_conn) in &[(10, 1000), (50, 200), (100, 100)] {
        let total_sends = num_conns * sends_per_conn;
        group.throughput(Throughput::Elements(total_sends as u64));
        group.bench_with_input(
            BenchmarkId::new("spawn_send", format!("{num_conns}x{sends_per_conn}")),
            &(num_conns, sends_per_conn),
            |b, &(n_conns, n_sends)| {
                b.iter_batched(
                    || {
                        let addr = find_free_addr();
                        runtime.block_on(async {
                            let listener = KcpListener::bind(&addr).await.unwrap();
                            let _guard = runtime.enter();
                            let peer: SocketAddr = "127.0.0.1:30000".parse().unwrap();
                            let conns: Vec<Arc<kcp2_std::KcpConnection>> = (0..n_conns)
                                .map(|i| listener.create_connection(i as u32, peer))
                                .collect();
                            conns
                        })
                    },
                    |conns| {
                        runtime.block_on(async {
                            let mut handles = Vec::with_capacity(conns.len());
                            for conn in &conns {
                                let conn = conn.clone();
                                handles.push(tokio::spawn(async move {
                                    for _ in 0..n_sends {
                                        conn.send(b"bench data").await.unwrap();
                                    }
                                }));
                            }
                            for h in handles {
                                h.await.unwrap();
                            }
                        });
                        black_box(());
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

#[allow(clippy::cast_sign_loss)]
fn bench_high_concurrency_tokio_mixed(c: &mut Criterion) {
    let runtime = rt();

    let mut group = c.benchmark_group("high_concurrency_tokio");

    for &(num_conns, ops_per_conn) in &[(10, 500), (50, 100)] {
        group.throughput(Throughput::Elements((num_conns * ops_per_conn) as u64));
        group.bench_with_input(
            BenchmarkId::new("spawn_mixed", format!("{num_conns}x{ops_per_conn}")),
            &(num_conns, ops_per_conn),
            |b, &(n_conns, n_ops)| {
                b.iter_batched(
                    || {
                        let addr = find_free_addr();
                        runtime.block_on(async {
                            let listener = KcpListener::bind(&addr).await.unwrap();
                            let _guard = runtime.enter();
                            let peer: SocketAddr = "127.0.0.1:30000".parse().unwrap();
                            let conns: Vec<Arc<kcp2_std::KcpConnection>> = (0..n_conns)
                                .map(|i| listener.create_connection(i as u32, peer))
                                .collect();
                            conns
                        })
                    },
                    |conns| {
                        runtime.block_on(async {
                            let mut handles = Vec::with_capacity(conns.len());
                            for conn in &conns {
                                let conn = conn.clone();
                                handles.push(tokio::spawn(async move {
                                    for _ in 0..n_ops {
                                        conn.send(b"data").await.unwrap();
                                        let mut buf = [0u8; 1024];
                                        let _ = conn.try_recv(&mut buf).await;
                                    }
                                }));
                            }
                            for h in handles {
                                h.await.unwrap();
                            }
                        });
                        black_box(());
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// criterion 配置
// ---------------------------------------------------------------------------

criterion_group! {
    name = core_concurrency;
    config = Criterion::default()
        .sample_size(20)
        .measurement_time(Duration::from_secs(15))
        .warm_up_time(Duration::from_secs(3));
    targets = bench_high_concurrency_send, bench_high_concurrency_input
}

criterion_group! {
    name = loopback_concurrency;
    config = Criterion::default()
        .sample_size(20)
        .measurement_time(Duration::from_secs(20))
        .warm_up_time(Duration::from_secs(3));
    targets = bench_high_concurrency_loopback, bench_high_concurrency_mixed
}

criterion_group! {
    name = listener_concurrency;
    config = Criterion::default()
        .sample_size(20)
        .measurement_time(Duration::from_secs(15))
        .warm_up_time(Duration::from_secs(3));
    targets = bench_high_concurrency_listener_create, bench_high_concurrency_listener_get, bench_high_concurrency_listener_remove
}

criterion_group! {
    name = tokio_concurrency;
    config = Criterion::default()
        .sample_size(15)
        .measurement_time(Duration::from_secs(25))
        .warm_up_time(Duration::from_secs(5));
    targets = bench_high_concurrency_tokio_send, bench_high_concurrency_tokio_mixed
}

criterion_main!(
    core_concurrency,
    loopback_concurrency,
    listener_concurrency,
    tokio_concurrency,
);

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use kcp2::KcpListener;
use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::runtime::Runtime;

fn rt() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| Runtime::new().unwrap())
}

fn find_free_addr() -> String {
    let sock = StdUdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = sock.local_addr().unwrap();
    drop(sock);
    format!("127.0.0.1:{}", addr.port())
}

/// 测量 `create_connection` 开销（bind 在 setup，不计入时间）
/// 使用 `iter_batched` 确保每次迭代都在全新 listener 上创建连接，
/// 避免 conv 重复插入导致测量覆盖/错误路径。
fn bench_connection_create(c: &mut Criterion) {
    let rt = rt();

    c.bench_function("listener_connection_create", |b| {
        b.iter_batched(
            || {
                let addr = find_free_addr();
                rt.block_on(async { KcpListener::bind(&addr).await.unwrap() })
            },
            |listener| {
                let _guard = rt.enter();
                let peer: SocketAddr = "127.0.0.1:30000".parse().unwrap();
                let _ = listener.create_connection(1, peer);
                black_box(listener.connection_count());
            },
            BatchSize::SmallInput,
        );
    });
}

/// 单连接查找延迟（预创建 listener + 连接）
fn bench_connection_lookup_single(c: &mut Criterion) {
    let rt = rt();

    c.bench_function("listener_lookup_single", |b| {
        let addr = find_free_addr();
        let listener = rt.block_on(async {
            let listener = KcpListener::bind(&addr).await.unwrap();
            let peer: SocketAddr = "127.0.0.1:30000".parse().unwrap();
            let _guard = rt.enter();
            listener.create_connection(1, peer);
            listener
        });

        b.iter(|| {
            let conn = listener.get_connection(1);
            black_box(conn);
        });
    });
}

/// 1k 连接下的查找延迟
#[allow(clippy::similar_names)]
fn bench_connection_lookup_1k(c: &mut Criterion) {
    let rt = rt();

    c.bench_function("listener_lookup_1k", |b| {
        let addr = find_free_addr();
        let listener = rt.block_on(async {
            let listener = KcpListener::bind(&addr).await.unwrap();
            let peer: SocketAddr = "127.0.0.1:30000".parse().unwrap();
            let _guard = rt.enter();
            for i in 1..=1000 {
                listener.create_connection(i, peer);
            }
            listener
        });

        let mut idx = 0u32;
        b.iter(|| {
            idx = idx.wrapping_add(1).max(1);
            let conv = idx % 1000 + 1;
            let conn = listener.get_connection(conv);
            black_box(conn);
        });
    });
}

/// 批量创建 100 连接（bind 在 setup，不计入时间）
fn bench_batch_create_100(c: &mut Criterion) {
    let rt = rt();

    let mut group = c.benchmark_group("batch_create_throughput");
    group.throughput(Throughput::Elements(100));
    group.bench_function("listener_batch_create_100", |b| {
        b.iter_batched_ref(
            || {
                let addr = find_free_addr();
                rt.block_on(async { KcpListener::bind(&addr).await.unwrap() })
            },
            |listener| {
                let _guard = rt.enter();
                let peer: SocketAddr = "127.0.0.1:30000".parse().unwrap();
                for i in 1..=100 {
                    listener.create_connection(i, peer);
                }
                black_box(listener.connection_count());
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

/// 批量创建 1k 连接（bind 在 setup，不计入时间）
fn bench_batch_create_1k(c: &mut Criterion) {
    let rt = rt();

    let mut group = c.benchmark_group("batch_create_throughput");
    group.throughput(Throughput::Elements(1000));
    group.bench_function("listener_batch_create_1k", |b| {
        b.iter_batched_ref(
            || {
                let addr = find_free_addr();
                rt.block_on(async { KcpListener::bind(&addr).await.unwrap() })
            },
            |listener| {
                let _guard = rt.enter();
                let peer: SocketAddr = "127.0.0.1:30000".parse().unwrap();
                for i in 1..=1000 {
                    listener.create_connection(i, peer);
                }
                black_box(listener.connection_count());
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

/// 移除 100 连接（setup 创建连接，routine 仅测量移除）
fn bench_connection_remove(c: &mut Criterion) {
    let rt = rt();

    let mut group = c.benchmark_group("remove_throughput");
    group.throughput(Throughput::Elements(100));
    group.bench_function("listener_connection_remove_100", |b| {
        b.iter_batched_ref(
            || {
                let addr = find_free_addr();
                let listener = rt.block_on(async { KcpListener::bind(&addr).await.unwrap() });
                let _guard = rt.enter();
                let peer: SocketAddr = "127.0.0.1:30000".parse().unwrap();
                for i in 0..100u32 {
                    listener.create_connection(i, peer);
                }
                listener
            },
            |listener| {
                for i in 0..100u32 {
                    listener.remove_connection(i);
                }
                black_box(listener.connection_count());
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

/// `DashMap` 并发查找（8 线程 × 1000 次随机查找）
fn bench_dashmap_concurrent(c: &mut Criterion) {
    let rt = rt();

    c.bench_function("listener_dashmap_concurrent", |b| {
        b.iter_batched_ref(
            || {
                let addr = find_free_addr();
                rt.block_on(async {
                    let listener = KcpListener::bind(&addr).await.unwrap();
                    let _guard = rt.enter();
                    let peer: SocketAddr = "127.0.0.1:30000".parse().unwrap();
                    for i in 0..100u32 {
                        listener.create_connection(i, peer);
                    }
                    Arc::new(listener)
                })
            },
            |listener| {
                rt.block_on(async {
                    let mut handles = Vec::new();
                    for _ in 0..8 {
                        let listener = listener.clone();
                        handles.push(tokio::spawn(async move {
                            for _ in 0..1000 {
                                let conv = rand::random::<u32>() % 100;
                                let _ = listener.get_connection(conv);
                            }
                        }));
                    }
                    for handle in handles {
                        handle.await.unwrap();
                    }
                });
            },
            BatchSize::SmallInput,
        );
    });
}

/// 测量 bind 开销（每次绑定新地址）
fn bench_listener_bind(c: &mut Criterion) {
    let rt = rt();

    c.bench_function("listener_bind", |b| {
        b.iter_batched(
            find_free_addr,
            |addr| {
                rt.block_on(async {
                    let listener = KcpListener::bind(&addr).await.unwrap();
                    black_box(listener.connection_count());
                });
            },
            BatchSize::SmallInput,
        );
    });
}

/// `allocate_conv` 开销（预创建 listener）
fn bench_allocate_conv(c: &mut Criterion) {
    let rt = rt();

    c.bench_function("listener_allocate_conv", |b| {
        let addr = find_free_addr();
        let listener = rt.block_on(async { KcpListener::bind(&addr).await.unwrap() });

        b.iter(|| {
            let conv = listener.allocate_conv();
            black_box(conv);
        });
    });
}

/// 并发 send 吞吐量（10 连接 × 100 次 send）
fn bench_concurrent_send(c: &mut Criterion) {
    let rt = rt();

    let mut group = c.benchmark_group("concurrent_send_throughput");
    group.throughput(Throughput::Bytes(10 * 100 * 10)); // 10 conns × 100 iters × 10 bytes
    group.bench_function("listener_concurrent_send", |b| {
        b.iter_batched_ref(
            || {
                let addr = find_free_addr();
                rt.block_on(async {
                    let listener = KcpListener::bind(&addr).await.unwrap();
                    let _guard = rt.enter();
                    let peer: SocketAddr = "127.0.0.1:30000".parse().unwrap();
                    let connections: Vec<Arc<kcp2::KcpConnection>> =
                        (0..10).map(|i| listener.create_connection(i, peer)).collect();
                    connections
                })
            },
            |connections| {
                rt.block_on(async {
                    let mut handles = Vec::new();
                    for conn in connections.iter() {
                        let conn = conn.clone();
                        handles.push(tokio::spawn(async move {
                            for _ in 0..100 {
                                conn.send(b"bench data").await.unwrap();
                            }
                        }));
                    }
                    for handle in handles {
                        handle.await.unwrap();
                    }
                });
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group! {
    name = connection_benches;
    config = Criterion::default().sample_size(30).measurement_time(Duration::from_secs(10));
    targets = bench_connection_create, bench_batch_create_100, bench_batch_create_1k, bench_connection_remove, bench_listener_bind
}

criterion_group! {
    name = lookup_benches;
    config = Criterion::default().sample_size(100);
    targets = bench_connection_lookup_single, bench_connection_lookup_1k, bench_allocate_conv
}

criterion_group! {
    name = concurrent_benches;
    config = Criterion::default().sample_size(20).measurement_time(Duration::from_secs(15));
    targets = bench_dashmap_concurrent, bench_concurrent_send
}

criterion_main!(connection_benches, lookup_benches, concurrent_benches);

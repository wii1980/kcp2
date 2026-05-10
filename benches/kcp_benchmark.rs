use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use kcp2::{Kcp, Segment};
use std::time::Duration;

fn bench_segment_encode(c: &mut Criterion) {
    let mut seg = Segment::new();
    seg.conv = 0x1122_3344;
    seg.cmd = 81;
    seg.frg = 0;
    seg.wnd = 128;
    seg.ts = 1000;
    seg.sn = 1;
    seg.una = 0;
    seg.data = vec![0u8; 100];

    c.bench_function("segment_encode", |b| {
        b.iter(|| {
            let mut buffer = [0u8; 200];
            seg.encode_to_slice(black_box(&mut buffer)).unwrap();
            black_box(&buffer);
        });
    });
}

fn bench_segment_decode(c: &mut Criterion) {
    let mut seg = Segment::new();
    seg.conv = 0x1122_3344;
    seg.cmd = 81;
    seg.frg = 0;
    seg.wnd = 128;
    seg.ts = 1000;
    seg.sn = 1;
    seg.una = 0;
    seg.data = vec![0u8; 100];

    let mut buffer = [0u8; 200];
    let used = seg.encode_to_slice(&mut buffer).unwrap();
    let encoded = &buffer[..used];

    c.bench_function("segment_decode", |b| {
        b.iter(|| {
            let _ = Segment::decode_from_slice(black_box(encoded)).unwrap();
        });
    });
}

fn bench_send_small_packet(c: &mut Criterion) {
    let data = vec![0u8; 100];

    let mut group = c.benchmark_group("send_throughput_misc");
    group.throughput(Throughput::Bytes(100));
    group.bench_function("send_small_packet", |b| {
        b.iter_batched_ref(
            || Kcp::new(0x1122_3344, |_: &[u8]| {}),
            |kcp| {
                kcp.send(black_box(&data)).unwrap();
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_send_large_packet(c: &mut Criterion) {
    let data = vec![0u8; 10 * 1024];

    let mut group = c.benchmark_group("send_throughput_misc");
    group.throughput(Throughput::Bytes(10 * 1024));
    group.bench_function("send_large_packet", |b| {
        b.iter_batched_ref(
            || {
                let mut kcp = Kcp::new(0x1122_3344, |_: &[u8]| {});
                kcp.set_mtu(1500).unwrap();
                kcp
            },
            |kcp| {
                kcp.send(black_box(&data)).unwrap();
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_input(c: &mut Criterion) {
    // Pre-encode segment once
    let mut seg = Segment::new();
    seg.conv = 0x1122_3344;
    seg.cmd = 81;
    seg.frg = 0;
    seg.wnd = 128;
    seg.ts = 1000;
    seg.sn = 1;
    seg.una = 0;
    seg.data = vec![0u8; 100];
    let mut buffer = [0u8; 200];
    let used = seg.encode_to_slice(&mut buffer).unwrap();
    let encoded = &buffer[..used];

    let mut group = c.benchmark_group("input_throughput");
    group.throughput(Throughput::Bytes(encoded.len() as u64));
    group.bench_function("input", |b| {
        b.iter_batched_ref(
            || Kcp::new(0x1122_3344, |_: &[u8]| {}),
            |kcp| {
                kcp.input(black_box(encoded)).unwrap();
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_loopback(c: &mut Criterion) {
    use std::cell::RefCell;
    use std::rc::Rc;

    let msg = b"test message";
    let mut group = c.benchmark_group("loopback_throughput");
    group.throughput(Throughput::Bytes(msg.len() as u64));
    group.bench_function("loopback", |b| {
        b.iter_batched_ref(
            || {
                let buf1: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
                let buf2: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
                let buf2_c = buf2.clone();
                let kcp1 = Kcp::new(0x1122_3344, move |data: &[u8]| {
                    buf2_c.borrow_mut().push(data.to_vec());
                });
                let buf1_c = buf1.clone();
                let kcp2 = Kcp::new(0x1122_3344, move |data: &[u8]| {
                    buf1_c.borrow_mut().push(data.to_vec());
                });
                (kcp1, kcp2, buf1, buf2)
            },
            |(kcp1, kcp2, _buf1, buf2)| {
                kcp1.send(msg).unwrap();
                kcp1.update(0);
                kcp1.flush();
                for pkt in buf2.borrow_mut().drain(..) {
                    kcp2.input(&pkt).unwrap();
                }
                let mut recv_buf = [0u8; 1024];
                let _ = kcp2.recv(&mut recv_buf).unwrap();
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

#[allow(clippy::cast_sign_loss, clippy::items_after_statements)]
fn bench_out_of_order(c: &mut Criterion) {
    let packet_count = 100;
    let mut packets = Vec::new();
    for i in 0..packet_count {
        let mut seg = Segment::new();
        seg.conv = 0x1122_3344;
        seg.cmd = 81;
        seg.frg = 0;
        seg.wnd = 256;
        seg.ts = i as u32 * 10;
        seg.sn = i as u32;
        seg.una = 0;
        seg.data = vec![0u8; 100];
        let mut buffer = vec![0u8; 200];
        let used = seg.encode_to_slice(&mut buffer).unwrap();
        packets.push(buffer[..used].to_vec());
    }
    use rand::seq::SliceRandom;
    use rand::thread_rng;
    packets.shuffle(&mut thread_rng());

    c.bench_function("out_of_order", |b| {
        b.iter_batched(
            || {
                let mut kcp = Kcp::new(0x1122_3344, |_: &[u8]| {});
                kcp.set_wndsize(256, 256);
                kcp
            },
            |mut kcp| {
                for packet in &packets {
                    kcp.input(black_box(packet)).unwrap();
                }
                black_box(kcp)
            },
            BatchSize::SmallInput,
        );
    });
}

#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn bench_multi_connection(c: &mut Criterion) {
    c.bench_function("multi_connection", |b| {
        b.iter_batched_ref(
            || {
                let mut connections = Vec::new();
                let payloads: Vec<Vec<u8>> = (0..10).map(|i| vec![i as u8; 100]).collect();
                for i in 0..10 {
                    let conv_id = 0x1000 + i as u32;
                    let mut kcp = Kcp::new(conv_id, |_: &[u8]| {});
                    kcp.set_wndsize(32, 32);
                    connections.push(kcp);
                }
                (connections, payloads)
            },
            |(connections, payloads)| {
                for (i, kcp) in connections.iter_mut().enumerate() {
                    kcp.send(&payloads[i]).unwrap();
                    kcp.update(0);
                    kcp.flush();
                }
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_recv(c: &mut Criterion) {
    let recv_data = b"test data for recv benchmark";
    let recv_len = recv_data.len() as u64;

    let mut group = c.benchmark_group("recv_throughput");
    group.throughput(Throughput::Bytes(recv_len));
    group.bench_function("recv", |b| {
        b.iter_batched_ref(
            || {
                let output: std::rc::Rc<core::cell::RefCell<Vec<Vec<u8>>>> =
                    std::rc::Rc::new(core::cell::RefCell::new(Vec::new()));
                let output_clone = output.clone();
                let mut kcp = Kcp::new(0x1122_3344, move |data: &[u8]| {
                    output_clone.borrow_mut().push(data.to_vec());
                });
                kcp.send(recv_data).unwrap();
                kcp.update(0);
                kcp.flush();
                let collected = output.borrow_mut().drain(..).collect::<Vec<_>>();
                for pkt in &collected {
                    kcp.input(pkt).unwrap();
                }
                kcp
            },
            |kcp| {
                let mut buf = [0u8; 1024];
                let _ = kcp.recv(&mut buf).unwrap();
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_flush(c: &mut Criterion) {
    c.bench_function("flush", |b| {
        b.iter_batched_ref(
            || {
                let mut kcp = Kcp::new(0x1122_3344, |_: &[u8]| {});
                kcp.send(b"flush benchmark data").unwrap();
                kcp.update(0); // marks updated=true, sets ts_flush
                kcp
            },
            |kcp| {
                kcp.flush();
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_update(c: &mut Criterion) {
    c.bench_function("update", |b| {
        b.iter_batched_ref(
            || {
                let mut kcp = Kcp::new(0x1122_3344, |_: &[u8]| {});
                kcp.send(b"update benchmark data").unwrap();
                kcp.update(0);
                kcp.flush();
                kcp
            },
            |kcp| {
                kcp.update(100);
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_send_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("send_throughput");
    for size in [64, 256, 1024, 1400] {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("send", size), &size, |b, &size| {
            let data = vec![0u8; size];
            b.iter_batched_ref(
                || Kcp::new(0x1122_3344, |_: &[u8]| {}),
                |kcp| {
                    kcp.send(black_box(&data)).unwrap();
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_segment_encode_sweep(c: &mut Criterion) {
    let mut group = c.benchmark_group("segment_encode_sweep");
    for size in [0, 64, 256, 1400] {
        let throughput = 24 + size; // header + data
        group.throughput(Throughput::Bytes(throughput as u64));
        group.bench_with_input(BenchmarkId::new("encode", size), &size, |b, &size| {
            let mut seg = Segment::new();
            seg.conv = 0x1122_3344;
            seg.cmd = 81;
            seg.wnd = 128;
            seg.ts = 1000;
            seg.sn = 1;
            if size > 0 {
                seg.data = vec![0u8; size];
            }
            b.iter(|| {
                let mut buffer = [0u8; 1500];
                seg.encode_to_slice(black_box(&mut buffer)).unwrap();
                black_box(&buffer);
            });
        });
    }
    group.finish();
}

fn bench_stream_mode(c: &mut Criterion) {
    let data = vec![0u8; 100];

    let mut group = c.benchmark_group("stream_throughput");
    group.throughput(Throughput::Bytes(100));
    group.bench_function("send_stream_mode", |b| {
        b.iter_batched_ref(
            || {
                let mut kcp = Kcp::new(0x1122_3344, |_: &[u8]| {});
                kcp.set_stream(true);
                kcp
            },
            |kcp| {
                kcp.send(black_box(&data)).unwrap();
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group! {
    name = segment_benches;
    config = Criterion::default().sample_size(100);
    targets = bench_segment_encode, bench_segment_decode, bench_segment_encode_sweep
}

criterion_group! {
    name = send_benches;
    config = Criterion::default().sample_size(50);
    targets = bench_send_small_packet, bench_send_large_packet, bench_send_throughput, bench_stream_mode
}

criterion_group! {
    name = protocol_benches;
    config = Criterion::default().sample_size(50);
    targets = bench_input, bench_recv, bench_flush, bench_update, bench_loopback
}

criterion_group! {
    name = scenario_benches;
    config = Criterion::default().sample_size(30).measurement_time(Duration::from_secs(10));
    targets = bench_out_of_order, bench_multi_connection
}

criterion_main!(
    segment_benches,
    send_benches,
    protocol_benches,
    scenario_benches
);

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::time::Duration;
use tokio::net::UdpSocket;

fn make_payload(size: usize) -> Vec<u8> {
    vec![0xABu8; size]
}

fn bench_udp_send_throughput(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut group = c.benchmark_group("transport_send");
    for &pkt_count in &[1, 8, 32] {
        let payload = make_payload(512);
        let payloads: Vec<Vec<u8>> = (0..pkt_count).map(|_| payload.clone()).collect();
        group.throughput(Throughput::Elements(pkt_count as u64));

        group.bench_with_input(
            BenchmarkId::new("udp_per_packet", pkt_count),
            &payloads,
            |b, payloads| {
                b.iter(|| {
                    rt.block_on(async {
                        let recv_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                        let target = recv_sock.local_addr().unwrap();
                        tokio::spawn(async move {
                            let mut buf = vec![0u8; 2048];
                            loop {
                                if tokio::time::timeout(Duration::from_millis(50), recv_sock.recv_from(&mut buf))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        });
                        let send_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                        for payload in payloads {
                            let _ = send_sock.try_send_to(payload, target);
                        }
                    });
                });
            },
        );
    }
    group.finish();
}

#[cfg(feature = "binger")]
mod binger_benches {
    use super::*;
    use kcp2_std::transport::BingerTransport;
    use binger_udp::batch::{RecvBatchRaw, SendBatchRaw};
    use binger_udp::{BingerUdp, Config};

    fn binger_batch_send(
        transport: &BingerTransport,
        payloads: &[Vec<u8>],
        target: std::net::SocketAddr,
    ) -> io::Result<()> {
        let mut batch = SendBatchRaw::with_capacity(payloads.len());
        for payload in payloads {
            batch.push(payload, Some(target)).map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        }
        transport.inner().try_send_batch(&mut batch)?;
        Ok(())
    }

    fn binger_batch_recv(binger: &BingerUdp, batch_size: usize) -> usize {
        let mut batch = RecvBatchRaw::with_capacity(batch_size, 2048);
        match binger.try_recv_batch(&mut batch) {
            Ok(n) => {
                for i in 0..n {
                    black_box(batch.data(i).len());
                }
                n
            }
            Err(_) => 0,
        }
    }

    pub fn bench_binger_send_throughput(c: &mut Criterion) {
        let rt = tokio::runtime::Runtime::new().unwrap();

        let mut group = c.benchmark_group("transport_send");
        for &pkt_count in &[1, 8, 32] {
            let payload = make_payload(512);
            let payloads: Vec<Vec<u8>> = (0..pkt_count).map(|_| payload.clone()).collect();
            group.throughput(Throughput::Elements(pkt_count as u64));

            group.bench_with_input(
                BenchmarkId::new("binger_batch", pkt_count),
                &payloads,
                |b, payloads| {
                    b.iter(|| {
                        rt.block_on(async {
                            let recv_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                            let target = recv_sock.local_addr().unwrap();
                            tokio::spawn(async move {
                                let mut buf = vec![0u8; 2048];
                                loop {
                                    if tokio::time::timeout(Duration::from_millis(50), recv_sock.recv_from(&mut buf))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            });

                            let std_sock = StdUdpSocket::bind("127.0.0.1:0").unwrap();
                            let binger = BingerUdp::from_std(std_sock, Config::new()).unwrap();
                            let transport = BingerTransport::new(binger);
                            binger_batch_send(&transport, payloads, target).unwrap();
                        });
                    });
                },
            );
        }
        group.finish();
    }

    pub fn bench_binger_recv_throughput(c: &mut Criterion) {
        let rt = tokio::runtime::Runtime::new().unwrap();

        let mut group = c.benchmark_group("transport_recv");
        for &batch_size in &[1, 8, 32] {
            group.throughput(Throughput::Elements(batch_size as u64));

            group.bench_with_input(
                BenchmarkId::new("binger_batch_recv", batch_size),
                &batch_size,
                |b, &batch_size| {
                    b.iter(|| {
                        rt.block_on(async {
                            let recv_std = StdUdpSocket::bind("127.0.0.1:0").unwrap();
                            let recv_addr = recv_std.local_addr().unwrap();
                            let binger = BingerUdp::from_std(recv_std, Config::new()).unwrap();

                            let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                            let payload = make_payload(256);
                            for _ in 0..batch_size {
                                let _ = sender.try_send_to(&payload, recv_addr);
                            }

                            binger_batch_recv(&binger, batch_size);
                        });
                    });
                },
            );
        }
        group.finish();
    }
}

#[cfg(feature = "binger")]
criterion_group! {
    name = send_benches;
    config = Criterion::default().sample_size(30).measurement_time(Duration::from_secs(8));
    targets = bench_udp_send_throughput, binger_benches::bench_binger_send_throughput
}

#[cfg(feature = "binger")]
criterion_group! {
    name = recv_benches;
    config = Criterion::default().sample_size(30).measurement_time(Duration::from_secs(8));
    targets = binger_benches::bench_binger_recv_throughput
}

#[cfg(feature = "binger")]
criterion_main!(send_benches, recv_benches);

#[cfg(not(feature = "binger"))]
criterion_group! {
    name = send_benches;
    config = Criterion::default().sample_size(30).measurement_time(Duration::from_secs(8));
    targets = bench_udp_send_throughput
}

#[cfg(not(feature = "binger"))]
criterion_main!(send_benches);

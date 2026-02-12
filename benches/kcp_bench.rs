//! Criterion benchmarks for KCP engine throughput and latency.

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use kcp_tokio::engine::KcpEngine;
use kcp_tokio::config::KcpConfig;

/// Perfect transfer: all packets from src delivered to dst.
fn transfer(src: &mut KcpEngine, dst: &mut KcpEngine) {
    for packet in src.drain_output() {
        let _ = dst.input(packet);
    }
}

/// Run bidirectional update/flush/transfer rounds, draining the receiver
/// each round to keep the receive window open.
fn run_rounds(kcp1: &mut KcpEngine, kcp2: &mut KcpEngine, rounds: usize) -> usize {
    let mut received = 0;
    for _ in 0..rounds {
        let _ = kcp1.update();
        let _ = kcp1.flush();
        transfer(kcp1, kcp2);

        while kcp2.recv().ok().flatten().is_some() {
            received += 1;
        }

        let _ = kcp2.update();
        let _ = kcp2.flush();
        transfer(kcp2, kcp1);
    }
    received
}

/// Drain all receivable messages.
fn drain_recv(engine: &mut KcpEngine) -> usize {
    let mut count = 0;
    while let Ok(Some(_)) = engine.recv() {
        count += 1;
    }
    count
}

fn engine_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("engine_throughput");

    for &msg_count in &[10, 100, 500] {
        let msg_size = 1024;
        group.throughput(Throughput::Bytes((msg_count * msg_size) as u64));

        group.bench_with_input(
            BenchmarkId::new("1KB_messages", msg_count),
            &msg_count,
            |b, &count| {
                b.iter(|| {
                    let config = KcpConfig::new().fast_mode().window_size(128, 128);
                    let mut kcp1 = KcpEngine::new(0xBEEF0001, config.clone());
                    let mut kcp2 = KcpEngine::new(0xBEEF0001, config);
                    kcp1.start().unwrap();
                    kcp2.start().unwrap();

                    let payload = Bytes::from(vec![0xABu8; msg_size]);
                    for _ in 0..count {
                        kcp1.send(payload.clone()).unwrap();
                    }
                    kcp1.flush().unwrap();

                    let mut received = run_rounds(&mut kcp1, &mut kcp2, count * 2);
                    received += drain_recv(&mut kcp2);
                    assert_eq!(received, count);
                });
            },
        );
    }

    group.finish();
}

fn engine_small_messages(c: &mut Criterion) {
    let mut group = c.benchmark_group("engine_small_messages");
    let msg_count = 1000;
    let msg_size = 64;
    group.throughput(Throughput::Elements(msg_count as u64));

    group.bench_function("64B_x_1000", |b| {
        b.iter(|| {
            let config = KcpConfig::new().fast_mode().window_size(128, 128);
            let mut kcp1 = KcpEngine::new(0xBEEF0002, config.clone());
            let mut kcp2 = KcpEngine::new(0xBEEF0002, config);
            kcp1.start().unwrap();
            kcp2.start().unwrap();

            let payload = Bytes::from(vec![0xCDu8; msg_size]);
            for _ in 0..msg_count {
                kcp1.send(payload.clone()).unwrap();
            }
            kcp1.flush().unwrap();

            let mut received = run_rounds(&mut kcp1, &mut kcp2, msg_count * 2);
            received += drain_recv(&mut kcp2);
            assert_eq!(received, msg_count);
        });
    });

    group.finish();
}

fn engine_large_message(c: &mut Criterion) {
    let mut group = c.benchmark_group("engine_large_message");

    for &size_kb in &[16, 64] {
        let size = size_kb * 1024;
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(
            BenchmarkId::new("single_message", format!("{}KB", size_kb)),
            &size,
            |b, &sz| {
                b.iter(|| {
                    let config = KcpConfig::new().fast_mode().window_size(256, 256);
                    let mut kcp1 = KcpEngine::new(0xBEEF0003, config.clone());
                    let mut kcp2 = KcpEngine::new(0xBEEF0003, config);
                    kcp1.start().unwrap();
                    kcp2.start().unwrap();

                    let payload: Vec<u8> = (0..sz).map(|i| (i % 256) as u8).collect();
                    kcp1.send(Bytes::from(payload)).unwrap();
                    kcp1.flush().unwrap();

                    let mut received = run_rounds(&mut kcp1, &mut kcp2, 200);
                    received += drain_recv(&mut kcp2);
                    assert_eq!(received, 1);
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, engine_throughput, engine_small_messages, engine_large_message);
criterion_main!(benches);

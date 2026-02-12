//! Resilience tests for KCP protocol: packet loss recovery, out-of-order
//! delivery, concurrent connections, and large message / sustained throughput.

mod common;

use bytes::Bytes;
use common::transfer;
use kcp_tokio::engine::KcpEngine;
use kcp_tokio::{KcpListener, KcpStream};
use kcp_tokio::config::KcpConfig;
use rand::seq::SliceRandom;
use rand::Rng;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::time::timeout;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Transfer all output packets from `src` to `dst`, dropping each packet
/// independently with probability `loss_rate`.
fn lossy_transfer(src: &mut KcpEngine, dst: &mut KcpEngine, loss_rate: f32) -> (usize, usize) {
    let mut rng = rand::thread_rng();
    let packets = src.drain_output();
    let total = packets.len();
    let mut delivered = 0;
    for packet in packets {
        if rng.gen::<f32>() >= loss_rate {
            let _ = dst.input(packet);
            delivered += 1;
        }
    }
    (total, delivered)
}

/// Transfer all output packets from `src` to `dst` in a random order.
fn reorder_transfer(src: &mut KcpEngine, dst: &mut KcpEngine) {
    let mut packets = src.drain_output();
    let mut rng = rand::thread_rng();
    packets.shuffle(&mut rng);
    for packet in packets {
        let _ = dst.input(packet);
    }
}

/// Transfer with both loss and reorder applied.
fn lossy_reorder_transfer(
    src: &mut KcpEngine,
    dst: &mut KcpEngine,
    loss_rate: f32,
) -> (usize, usize) {
    let mut rng = rand::thread_rng();
    let mut packets = src.drain_output();
    packets.shuffle(&mut rng);
    let total = packets.len();
    let mut delivered = 0;
    for packet in packets {
        if rng.gen::<f32>() >= loss_rate {
            let _ = dst.input(packet);
            delivered += 1;
        }
    }
    (total, delivered)
}

/// Drive both engines through multiple update/flush/transfer rounds so that
/// retransmissions can happen and data can be fully delivered.
/// Does NOT drain the receiver — messages accumulate in kcp2's receive queue.
fn run_rounds(
    kcp1: &mut KcpEngine,
    kcp2: &mut KcpEngine,
    rounds: usize,
    loss_rate: f32,
    reorder: bool,
) {
    for _ in 0..rounds {
        let _ = kcp1.update();
        let _ = kcp1.flush();
        do_transfer(kcp1, kcp2, loss_rate, reorder);

        let _ = kcp2.update();
        let _ = kcp2.flush();
        do_transfer(kcp2, kcp1, loss_rate, reorder);
    }
}

/// Same as `run_rounds`, but also drains received messages from `kcp2` each
/// round to keep the receive window open (simulates an application reading).
fn run_rounds_draining(
    kcp1: &mut KcpEngine,
    kcp2: &mut KcpEngine,
    rounds: usize,
    loss_rate: f32,
    reorder: bool,
    recv_sink: &mut Vec<Bytes>,
) {
    for _ in 0..rounds {
        let _ = kcp1.update();
        let _ = kcp1.flush();
        do_transfer(kcp1, kcp2, loss_rate, reorder);

        while let Ok(Some(msg)) = kcp2.recv() {
            recv_sink.push(msg);
        }

        let _ = kcp2.update();
        let _ = kcp2.flush();
        do_transfer(kcp2, kcp1, loss_rate, reorder);
    }
}

/// Apply the appropriate transfer strategy between two engines.
fn do_transfer(src: &mut KcpEngine, dst: &mut KcpEngine, loss_rate: f32, reorder: bool) {
    if reorder {
        lossy_reorder_transfer(src, dst, loss_rate);
    } else if loss_rate > 0.0 {
        lossy_transfer(src, dst, loss_rate);
    } else {
        transfer(src, dst);
    }
}

/// Collect all available messages from the engine.
fn drain_recv(engine: &mut KcpEngine) -> Vec<Bytes> {
    let mut msgs = Vec::new();
    while let Ok(Some(msg)) = engine.recv() {
        msgs.push(msg);
    }
    msgs
}

// ---------------------------------------------------------------------------
// P0: Packet loss recovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_packet_loss_recovery() {
    let config = KcpConfig::new().turbo_mode().window_size(64, 64);
    let mut kcp1 = KcpEngine::new(0xAAAA0001, config.clone());
    let mut kcp2 = KcpEngine::new(0xAAAA0001, config);

    kcp1.start().unwrap();
    kcp2.start().unwrap();

    let messages: Vec<Vec<u8>> = (0..10)
        .map(|i| format!("loss-test-message-{:04}", i).into_bytes())
        .collect();

    // Send all messages
    for msg in &messages {
        kcp1.send(Bytes::from(msg.clone())).unwrap();
    }
    kcp1.flush().unwrap();

    // Run rounds with 30% packet loss. turbo_mode disables congestion control
    // so the send window stays open, allowing fast retransmit to work.
    for _ in 0..30 {
        run_rounds(&mut kcp1, &mut kcp2, 30, 0.3, false);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let received = drain_recv(&mut kcp2);
    assert_eq!(
        received.len(),
        messages.len(),
        "Expected {} messages, got {} (some lost and not retransmitted)",
        messages.len(),
        received.len(),
    );

    for (i, msg) in received.iter().enumerate() {
        assert_eq!(
            msg.as_ref(),
            messages[i].as_slice(),
            "Message {} content mismatch",
            i,
        );
    }

    // Confirm retransmissions actually occurred (timeout or fast retransmit)
    let stats = kcp1.stats();
    assert!(
        stats.retransmissions > 0
            || stats.fast_retransmissions > 0
            || stats.packets_sent > messages.len() as u64,
        "Expected retransmissions: rto={}, fast={}, sent={}",
        stats.retransmissions,
        stats.fast_retransmissions,
        stats.packets_sent,
    );
}

// ---------------------------------------------------------------------------
// P0: Out-of-order delivery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_out_of_order_delivery() {
    // Small MTU forces fragmentation: 300-byte message / ~76 MSS = 4+ packets
    let config = KcpConfig::new().fast_mode().mtu(100);
    let mut kcp1 = KcpEngine::new(0xBBBB0001, config.clone());
    let mut kcp2 = KcpEngine::new(0xBBBB0001, config);

    kcp1.start().unwrap();
    kcp2.start().unwrap();

    let message: Vec<u8> = (0u8..=255).cycle().take(300).collect();
    kcp1.send(Bytes::from(message.clone())).unwrap();
    kcp1.flush().unwrap();

    // Deliver packets in shuffled order
    reorder_transfer(&mut kcp1, &mut kcp2);
    let _ = kcp2.update();

    // May need extra rounds for the engine to reassemble
    run_rounds(&mut kcp1, &mut kcp2, 20, 0.0, false);

    let received = drain_recv(&mut kcp2);
    assert_eq!(received.len(), 1, "Expected exactly 1 reassembled message");
    assert_eq!(
        received[0].as_ref(),
        message.as_slice(),
        "Reassembled message content mismatch",
    );
}

// ---------------------------------------------------------------------------
// P0: Combined loss + reorder
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_loss_and_reorder_combined() {
    let config = KcpConfig::new().turbo_mode().mtu(200).window_size(64, 64);
    let mut kcp1 = KcpEngine::new(0xCCCC0001, config.clone());
    let mut kcp2 = KcpEngine::new(0xCCCC0001, config);

    kcp1.start().unwrap();
    kcp2.start().unwrap();

    let messages: Vec<Vec<u8>> = (0..5)
        .map(|i| format!("combo-msg-{:04}-{}", i, "x".repeat(80)).into_bytes())
        .collect();

    for msg in &messages {
        kcp1.send(Bytes::from(msg.clone())).unwrap();
    }
    kcp1.flush().unwrap();

    // 30% loss + reorder on both directions; turbo_mode keeps window open.
    // Drain receiver each round to prevent backpressure.
    let mut received = Vec::new();
    for _ in 0..40 {
        run_rounds_draining(&mut kcp1, &mut kcp2, 30, 0.3, true, &mut received);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    received.extend(drain_recv(&mut kcp2));

    assert_eq!(
        received.len(),
        messages.len(),
        "Expected {} messages under loss+reorder, got {}",
        messages.len(),
        received.len(),
    );

    for (i, msg) in received.iter().enumerate() {
        assert_eq!(msg.as_ref(), messages[i].as_slice());
    }
}

// ---------------------------------------------------------------------------
// P1: Concurrent connections (async-level)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_connections() {
    let server_config = KcpConfig::new().fast_mode();
    let (addr_tx, addr_rx) = oneshot::channel();

    let server_handle = tokio::spawn(async move {
        let mut listener = KcpListener::bind("127.0.0.1:0".parse().unwrap(), server_config)
            .await
            .expect("Failed to bind");

        addr_tx.send(*listener.local_addr()).unwrap();

        // Accept 5 clients and echo their messages back
        for _ in 0..5 {
            let (mut stream, _peer) = timeout(Duration::from_secs(10), listener.accept())
                .await
                .expect("Accept timeout")
                .expect("Accept failed");

            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let n = timeout(Duration::from_secs(5), stream.read(&mut buf))
                    .await
                    .expect("Read timeout")
                    .expect("Read failed");

                stream.write_all(&buf[..n]).await.expect("Write failed");
                stream.flush().await.expect("Flush failed");
                let _ = stream.close().await;
            });
        }

        // Keep listener alive while clients finish
        tokio::time::sleep(Duration::from_secs(3)).await;
        let _ = listener.close().await;
    });

    let server_addr = addr_rx.await.unwrap();

    // Spawn 5 clients concurrently
    let mut client_handles = Vec::new();
    for i in 0..5u32 {
        let addr = server_addr;
        client_handles.push(tokio::spawn(async move {
            let config = KcpConfig::new().fast_mode();
            let mut client = timeout(
                Duration::from_secs(5),
                KcpStream::connect(addr, config),
            )
            .await
            .expect("Connect timeout")
            .expect("Connect failed");

            let msg = format!("client-{}", i);
            client
                .write_all(msg.as_bytes())
                .await
                .expect("Send failed");
            client.flush().await.expect("Flush failed");

            let mut buf = [0u8; 1024];
            let n = timeout(Duration::from_secs(5), client.read(&mut buf))
                .await
                .expect("Read timeout")
                .expect("Read failed");

            let response = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = client.close().await;

            (i, msg, response)
        }));
    }

    // Verify each client got its own message echoed back
    for handle in client_handles {
        let (id, sent, received) = timeout(Duration::from_secs(10), handle)
            .await
            .expect("Task timeout")
            .expect("Task panicked");
        assert_eq!(
            sent, received,
            "Client {} echo mismatch: sent '{}', got '{}'",
            id, sent, received,
        );
    }

    let _ = timeout(Duration::from_secs(5), server_handle).await;
}

// ---------------------------------------------------------------------------
// P1: Large message delivery (engine-level)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_large_message_delivery() {
    let config = KcpConfig::new().fast_mode().window_size(128, 128);
    let mut kcp1 = KcpEngine::new(0xDDDD0001, config.clone());
    let mut kcp2 = KcpEngine::new(0xDDDD0001, config);

    kcp1.start().unwrap();
    kcp2.start().unwrap();

    // 64 KB message — will be split into ~47 fragments at default MTU
    let large_msg: Vec<u8> = (0..65536).map(|i| (i % 256) as u8).collect();
    kcp1.send(Bytes::from(large_msg.clone())).unwrap();
    kcp1.flush().unwrap();

    // Multiple rounds needed: window size limits how many fragments fly at once
    run_rounds(&mut kcp1, &mut kcp2, 100, 0.0, false);

    let received = drain_recv(&mut kcp2);
    assert_eq!(received.len(), 1, "Expected 1 reassembled large message");
    assert_eq!(
        received[0].len(),
        large_msg.len(),
        "Large message size mismatch: expected {}, got {}",
        large_msg.len(),
        received[0].len(),
    );
    assert_eq!(received[0].as_ref(), large_msg.as_slice());
}

// ---------------------------------------------------------------------------
// P1: Sustained throughput (engine-level, exceeds send window)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_sustained_throughput() {
    // Default snd_wnd = 32; we send 100 messages to exceed it
    let config = KcpConfig::new().fast_mode().window_size(32, 128);
    let mut kcp1 = KcpEngine::new(0xEEEE0001, config.clone());
    let mut kcp2 = KcpEngine::new(0xEEEE0001, config);

    kcp1.start().unwrap();
    kcp2.start().unwrap();

    let msg_count = 100;
    let messages: Vec<Vec<u8>> = (0..msg_count)
        .map(|i| {
            let mut data = vec![0u8; 1024];
            // Tag each message for identification
            let tag = format!("msg-{:04}", i);
            data[..tag.len()].copy_from_slice(tag.as_bytes());
            data
        })
        .collect();

    // Send all messages (will exceed window, engine must buffer)
    for msg in &messages {
        kcp1.send(Bytes::from(msg.clone())).unwrap();
    }
    kcp1.flush().unwrap();

    // Many bidirectional rounds: data flows kcp1→kcp2, ACKs flow kcp2→kcp1,
    // which opens the send window for more data. Must drain receiver each
    // round to keep the receive window open (rcv_queue.len() < rcv_wnd).
    let mut received = Vec::new();
    run_rounds_draining(&mut kcp1, &mut kcp2, 500, 0.0, false, &mut received);
    received.extend(drain_recv(&mut kcp2));

    assert_eq!(
        received.len(),
        msg_count,
        "Expected {} messages, got {} (flow control issue)",
        msg_count,
        received.len(),
    );

    for (i, msg) in received.iter().enumerate() {
        assert_eq!(
            msg.as_ref(),
            messages[i].as_slice(),
            "Message {} content mismatch",
            i,
        );
    }
}

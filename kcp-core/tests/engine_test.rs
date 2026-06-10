//! Core-only integration tests — no tokio dependency

use bytes::Bytes;
use kcp_core::{KcpCoreConfig, KcpEngine};

/// Flush, then move all output packets from one engine into another's input.
/// (`send` only queues — emission is driven by `flush`, as in canonical KCP.)
fn transfer(src: &mut KcpEngine, dst: &mut KcpEngine) {
    src.flush().unwrap();
    for packet in src.drain_output() {
        let _ = dst.input(packet);
    }
}

#[test]
fn test_basic_send_recv() {
    let config = KcpCoreConfig::default();
    let mut client = KcpEngine::new(1, config.clone());
    let mut server = KcpEngine::new(1, config);

    client.start().unwrap();
    server.start().unwrap();

    // Client sends data
    client.send(Bytes::from("hello")).unwrap();

    // Transfer packets: client → server
    transfer(&mut client, &mut server);

    // Server receives
    let msg = server.recv().unwrap().expect("should receive data");
    assert_eq!(msg, Bytes::from("hello"));

    // Transfer ACKs back: server → client
    transfer(&mut server, &mut client);
}

#[test]
fn test_stats() {
    let config = KcpCoreConfig::default();
    let mut client = KcpEngine::new(2, config.clone());
    let mut server = KcpEngine::new(2, config);

    client.start().unwrap();
    server.start().unwrap();

    client.send(Bytes::from("stats test")).unwrap();
    transfer(&mut client, &mut server);

    let _ = server.recv().unwrap();
    transfer(&mut server, &mut client);

    let stats = client.stats();
    assert!(stats.bytes_sent > 0);
    assert!(stats.packets_sent > 0);

    let stats = server.stats();
    assert!(stats.bytes_received > 0);
    assert!(stats.packets_received > 0);
}

#[test]
fn test_large_message() {
    let config = KcpCoreConfig::default();
    let mut client = KcpEngine::new(3, config.clone());
    let mut server = KcpEngine::new(3, config);

    client.start().unwrap();
    server.start().unwrap();

    // Send a message larger than MSS (1400 - 24 = 1376 bytes)
    let data = vec![0xABu8; 4000];
    client.send(Bytes::from(data.clone())).unwrap();

    // Transfer all output (multiple fragments)
    transfer(&mut client, &mut server);

    let msg = server.recv().unwrap().expect("should receive large message");
    assert_eq!(msg.len(), 4000);
    assert_eq!(&msg[..], &data[..]);
}

#[test]
fn test_malformed_frg_does_not_panic() {
    // A peer-controlled PUSH segment with frg = 255 must not overflow when
    // peek_size() computes (frg + 1). Regression for a u8 add-overflow that
    // panicked in debug builds and wrapped to 0 in release.
    let config = KcpCoreConfig::default();
    let mut server = KcpEngine::new(1, config);
    server.start().unwrap();

    let mut pkt = Vec::with_capacity(25);
    pkt.extend_from_slice(&1u32.to_le_bytes()); // conv = 1
    pkt.push(81); // cmd = IKCP_CMD_PUSH
    pkt.push(255); // frg = 255 (malicious)
    pkt.extend_from_slice(&128u16.to_le_bytes()); // wnd
    pkt.extend_from_slice(&0u32.to_le_bytes()); // ts
    pkt.extend_from_slice(&0u32.to_le_bytes()); // sn = 0 (== rcv_nxt)
    pkt.extend_from_slice(&0u32.to_le_bytes()); // una
    pkt.extend_from_slice(&1u32.to_le_bytes()); // len = 1
    pkt.push(0x42); // 1 data byte

    server.input(Bytes::from(pkt)).unwrap();

    // Must not panic; the fragment claims frg=255 so the message is incomplete.
    let msg = server.recv().unwrap();
    assert!(msg.is_none());
}

#[test]
fn test_conv_mismatch() {
    let config = KcpCoreConfig::default();
    let mut client = KcpEngine::new(100, config.clone());
    let mut server = KcpEngine::new(999, config); // different conv

    client.start().unwrap();
    server.start().unwrap();

    client.send(Bytes::from("mismatch")).unwrap();

    // Transfer packets — server has different conv, should ignore
    transfer(&mut client, &mut server);

    let msg = server.recv().unwrap();
    assert!(msg.is_none(), "server should not receive data with mismatched conv");
}

#[test]
fn test_tiny_mtu_does_not_underflow_mss() {
    // mtu at or below the 24-byte header used to underflow `mtu - IKCP_OVERHEAD`
    // in mss() (debug panic, ~4G mss in release). new() now clamps the mtu.
    let config = KcpCoreConfig {
        mtu: 10,
        ..Default::default()
    };
    let mut client = KcpEngine::new(7, config.clone());
    let mut server = KcpEngine::new(7, config);

    client.send(Bytes::from("tiny-mtu payload")).unwrap();
    transfer(&mut client, &mut server);

    // mss is clamped to 1 byte: the message arrives heavily fragmented but intact.
    let msg = server.recv().unwrap().expect("should receive data");
    assert_eq!(msg, Bytes::from("tiny-mtu payload"));
}

#[test]
fn test_fragment_count_exceeding_recv_window_fails_fast() {
    // A message needing more fragments than the receive window can hold can
    // never be reassembled on a symmetrically-configured peer (the receive
    // queue fills before the fragment chain completes — protocol deadlock).
    // send() must reject it up front instead.
    let config = KcpCoreConfig {
        rcv_wnd: 16,
        ..Default::default()
    };
    let mut engine = KcpEngine::new(8, config);

    let mss = 1400 - 24;
    assert!(
        engine.send(Bytes::from(vec![0u8; mss * 17])).is_err(),
        "17 fragments must be rejected for rcv_wnd=16"
    );
    assert!(
        engine.send(Bytes::from(vec![0u8; mss * 16])).is_ok(),
        "16 fragments must still be accepted for rcv_wnd=16"
    );
}

#[test]
fn test_small_messages_share_datagrams() {
    // send() only queues; one flush must pack queued small messages into
    // shared MTU-sized datagrams (10 × (24 + 10) = 340 bytes → 1 datagram)
    // instead of emitting one datagram per message.
    let config = KcpCoreConfig::default();
    let mut client = KcpEngine::new(9, config);
    client.start().unwrap();

    for _ in 0..10 {
        client.send(Bytes::from_static(b"0123456789")).unwrap();
    }
    assert!(
        client.drain_output().is_empty(),
        "send() must not emit before flush()"
    );

    client.flush().unwrap();
    let packets = client.drain_output();
    assert_eq!(
        packets.len(),
        1,
        "10 small messages must share one MTU-packed datagram, got {}",
        packets.len()
    );
}

#[test]
fn test_stream_mode_merge() {
    // Stream mode merges consecutive small sends into one segment (and the
    // in-place append path must keep the bytes intact and in order).
    let config = KcpCoreConfig {
        stream_mode: true,
        ..Default::default()
    };
    let mut client = KcpEngine::new(10, config.clone());
    let mut server = KcpEngine::new(10, config);
    client.start().unwrap();
    server.start().unwrap();

    let mut expected = Vec::new();
    for i in 0..200u32 {
        let chunk = format!("chunk-{i:03}|");
        expected.extend_from_slice(chunk.as_bytes());
        client.send(Bytes::from(chunk)).unwrap();
    }

    transfer(&mut client, &mut server);
    transfer(&mut server, &mut client); // ACKs
    transfer(&mut client, &mut server); // any remainder

    let mut received = Vec::new();
    while let Some(chunk) = server.recv().unwrap() {
        received.extend_from_slice(&chunk);
    }
    assert_eq!(received, expected, "stream-mode merge corrupted data");

    // 200 × 10B = 2000B merged into mss-sized segments → far fewer than 200.
    assert!(
        server.stats().packets_received < 20,
        "merge ineffective: {} packets for 2000 bytes",
        server.stats().packets_received
    );
}

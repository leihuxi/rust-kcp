//! Core-only integration tests — no tokio dependency

use bytes::Bytes;
use kcp_core::{KcpCoreConfig, KcpEngine};

/// Send all output packets from one engine into another engine's input.
fn transfer(src: &mut KcpEngine, dst: &mut KcpEngine) {
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

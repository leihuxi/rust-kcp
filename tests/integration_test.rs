//! Integration tests for async KCP implementation

use bytes::Bytes;
use kcp_tokio::async_kcp::engine::KcpEngine;
use kcp_tokio::config::KcpConfig;

/// Helper: send all output from one engine into another engine's input,
/// simulating a network transfer.
fn transfer(src: &mut KcpEngine, dst: &mut KcpEngine) {
    for packet in src.drain_output() {
        dst.input(packet).unwrap();
    }
}

#[tokio::test]
async fn test_async_kcp_basic_send_recv() {
    let config = KcpConfig::new().fast_mode();
    let mut kcp1 = KcpEngine::new(0x12345678, config.clone());
    let mut kcp2 = KcpEngine::new(0x12345678, config);

    kcp1.start().unwrap();
    kcp2.start().unwrap();

    // Send message from kcp1
    let message = b"Hello async KCP!";
    kcp1.send(Bytes::from(message.as_slice())).unwrap();
    kcp1.flush().unwrap();

    // Transfer network data to kcp2
    transfer(&mut kcp1, &mut kcp2);

    // Update kcp2 and receive
    kcp2.update().unwrap();

    let received = kcp2.recv().unwrap();
    assert!(received.is_some());
    assert_eq!(&received.unwrap()[..], message);
}

#[tokio::test]
async fn test_async_kcp_fragmentation() {
    // Create engines with small MTU to force fragmentation
    let config = KcpConfig::new().mtu(150);
    let mut kcp1 = KcpEngine::new(0x11111111, config.clone());
    let mut kcp2 = KcpEngine::new(0x11111111, config);

    kcp1.start().unwrap();
    kcp2.start().unwrap();

    // Large message that will be fragmented
    let large_message: Vec<u8> = (0..500).map(|i| (i % 256) as u8).collect();

    kcp1.send(Bytes::from(large_message.clone())).unwrap();
    kcp1.flush().unwrap();

    transfer(&mut kcp1, &mut kcp2);

    kcp2.update().unwrap();

    // Receive reassembled message
    let received = kcp2.recv().unwrap();
    assert!(received.is_some());
    assert_eq!(received.unwrap().to_vec(), large_message);
}

#[tokio::test]
async fn test_async_kcp_multiple_messages() {
    let config = KcpConfig::new().fast_mode();
    let mut kcp1 = KcpEngine::new(0x33333333, config.clone());
    let mut kcp2 = KcpEngine::new(0x33333333, config);

    kcp1.start().unwrap();
    kcp2.start().unwrap();

    let messages = vec![
        b"First message".as_slice(),
        b"Second message",
        b"Third message",
    ];

    // Send multiple messages
    for message in &messages {
        kcp1.send(Bytes::from(*message)).unwrap();
    }
    kcp1.flush().unwrap();

    transfer(&mut kcp1, &mut kcp2);

    kcp2.update().unwrap();

    // Receive all messages
    for expected in &messages {
        let received = kcp2.recv().unwrap();
        assert!(received.is_some());
        assert_eq!(&received.unwrap()[..], *expected);
    }

    // No more messages
    let no_more = kcp2.recv().unwrap();
    assert!(no_more.is_none());
}

#[tokio::test]
async fn test_async_kcp_configuration() {
    let config = KcpConfig::new().fast_mode().window_size(128, 128).mtu(1400);

    let engine = KcpEngine::new(0x44444444, config);
    let stats = engine.stats();

    // Verify initial state
    assert_eq!(stats.bytes_sent, 0);
    assert_eq!(stats.bytes_received, 0);
    assert_eq!(stats.packets_sent, 0);
    assert_eq!(stats.packets_received, 0);
}

#[tokio::test]
async fn test_async_kcp_bidirectional() {
    let config = KcpConfig::new().fast_mode();
    let mut kcp1 = KcpEngine::new(0x55555555, config.clone());
    let mut kcp2 = KcpEngine::new(0x55555555, config);

    kcp1.start().unwrap();
    kcp2.start().unwrap();

    // Send from kcp1 to kcp2
    let message1 = b"Hello from KCP1";
    kcp1.send(Bytes::from(message1.as_slice())).unwrap();
    kcp1.flush().unwrap();

    // Transfer 1->2
    transfer(&mut kcp1, &mut kcp2);
    kcp2.update().unwrap();

    // Send response from kcp2 to kcp1
    let message2 = b"Hello back from KCP2";
    kcp2.send(Bytes::from(message2.as_slice())).unwrap();
    kcp2.flush().unwrap();

    // Transfer 2->1
    transfer(&mut kcp2, &mut kcp1);
    kcp1.update().unwrap();

    // Verify both received correctly
    let received1 = kcp2.recv().unwrap();
    assert!(received1.is_some());
    assert_eq!(&received1.unwrap()[..], message1);

    let received2 = kcp1.recv().unwrap();
    assert!(received2.is_some());
    assert_eq!(&received2.unwrap()[..], message2);
}

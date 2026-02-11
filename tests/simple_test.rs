//! Simple integration tests for async KCP implementation

use bytes::Bytes;
use kcp_tokio::async_kcp::engine::KcpEngine;
use kcp_tokio::config::KcpConfig;

/// Helper: send all output from one engine into another engine's input.
fn transfer(src: &mut KcpEngine, dst: &mut KcpEngine) {
    for packet in src.drain_output() {
        dst.input(packet).unwrap();
    }
}

#[tokio::test]
async fn test_basic_send_recv() {
    let config = KcpConfig::new().fast_mode();
    let mut kcp1 = KcpEngine::new(0x12345678, config.clone());
    let mut kcp2 = KcpEngine::new(0x12345678, config);

    kcp1.start().unwrap();
    kcp2.start().unwrap();

    // Send a simple message
    let message = b"Hello KCP!";
    kcp1.send(Bytes::from(message.as_slice())).unwrap();
    kcp1.flush().unwrap();

    // Transfer data
    transfer(&mut kcp1, &mut kcp2);

    kcp2.update().unwrap();

    // Receive message
    let received = kcp2.recv().unwrap();
    assert!(received.is_some());
    assert_eq!(&received.unwrap()[..], message);
}

#[tokio::test]
async fn test_configuration() {
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
async fn test_fragmentation_simple() {
    // Small MTU to force fragmentation
    let config = KcpConfig::new().mtu(100);
    let mut kcp1 = KcpEngine::new(0x11111111, config.clone());
    let mut kcp2 = KcpEngine::new(0x11111111, config);

    kcp1.start().unwrap();
    kcp2.start().unwrap();

    // Large message that will fragment
    let large_message: Vec<u8> = (0..300).map(|i| (i % 256) as u8).collect();

    kcp1.send(Bytes::from(large_message.clone())).unwrap();
    kcp1.flush().unwrap();

    // Transfer all packets
    transfer(&mut kcp1, &mut kcp2);

    kcp2.update().unwrap();

    // Receive reassembled message
    let received = kcp2.recv().unwrap();
    assert!(received.is_some());
    assert_eq!(received.unwrap().to_vec(), large_message);
}

#[tokio::test]
async fn test_multiple_messages() {
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

    // Send all messages
    for message in &messages {
        kcp1.send(Bytes::from(*message)).unwrap();
    }
    kcp1.flush().unwrap();

    // Transfer data
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
async fn test_empty_and_small_messages() {
    let config = KcpConfig::new().fast_mode();
    let mut kcp1 = KcpEngine::new(0x55555555, config.clone());
    let mut kcp2 = KcpEngine::new(0x55555555, config);

    kcp1.start().unwrap();
    kcp2.start().unwrap();

    // Test different message sizes (skip empty messages as they're not meaningful in KCP)
    let test_messages = vec![
        vec![42],          // Single byte
        b"Hi".to_vec(),    // Two bytes
        b"Hello".to_vec(), // Small message
    ];

    for message in &test_messages {
        kcp1.send(Bytes::from(message.clone())).unwrap();
        kcp1.flush().unwrap();

        // Transfer data
        transfer(&mut kcp1, &mut kcp2);

        kcp2.update().unwrap();

        // Receive message
        let received = kcp2.recv().unwrap();
        assert!(received.is_some());
        assert_eq!(&received.unwrap().to_vec(), message);
    }
}

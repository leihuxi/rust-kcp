//! Simple integration tests for async KCP implementation

mod common;

use bytes::Bytes;
use common::transfer;
use kcp_tokio::engine::KcpEngine;
use kcp_tokio::config::KcpConfig;

#[tokio::test]
async fn test_basic_send_recv() {
    let config = KcpConfig::new().fast_mode();
    let mut kcp1 = KcpEngine::new(0x12345678, config.clone());
    let mut kcp2 = KcpEngine::new(0x12345678, config);

    kcp1.start().unwrap();
    kcp2.start().unwrap();

    let message = b"Hello KCP!";
    kcp1.send(Bytes::from(message.as_slice())).unwrap();
    kcp1.flush().unwrap();

    transfer(&mut kcp1, &mut kcp2);
    kcp2.update().unwrap();

    let received = kcp2.recv().unwrap();
    assert!(received.is_some());
    assert_eq!(&received.unwrap()[..], message);
}

#[tokio::test]
async fn test_configuration() {
    let config = KcpConfig::new().fast_mode().window_size(128, 128).mtu(1400);

    let engine = KcpEngine::new(0x44444444, config);
    let stats = engine.stats();

    assert_eq!(stats.bytes_sent, 0);
    assert_eq!(stats.bytes_received, 0);
    assert_eq!(stats.packets_sent, 0);
    assert_eq!(stats.packets_received, 0);
}

#[tokio::test]
async fn test_fragmentation_simple() {
    let config = KcpConfig::new().mtu(100);
    let mut kcp1 = KcpEngine::new(0x11111111, config.clone());
    let mut kcp2 = KcpEngine::new(0x11111111, config);

    kcp1.start().unwrap();
    kcp2.start().unwrap();

    let large_message: Vec<u8> = (0..300).map(|i| (i % 256) as u8).collect();

    kcp1.send(Bytes::from(large_message.clone())).unwrap();
    kcp1.flush().unwrap();

    transfer(&mut kcp1, &mut kcp2);
    kcp2.update().unwrap();

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

    for message in &messages {
        kcp1.send(Bytes::from(*message)).unwrap();
    }
    kcp1.flush().unwrap();

    transfer(&mut kcp1, &mut kcp2);
    kcp2.update().unwrap();

    for expected in &messages {
        let received = kcp2.recv().unwrap();
        assert!(received.is_some());
        assert_eq!(&received.unwrap()[..], *expected);
    }

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

    let test_messages = vec![
        vec![42],          // Single byte
        b"Hi".to_vec(),    // Two bytes
        b"Hello".to_vec(), // Small message
    ];

    for message in &test_messages {
        kcp1.send(Bytes::from(message.clone())).unwrap();
        kcp1.flush().unwrap();

        transfer(&mut kcp1, &mut kcp2);
        kcp2.update().unwrap();

        let received = kcp2.recv().unwrap();
        assert!(received.is_some());
        assert_eq!(&received.unwrap().to_vec(), message);
    }
}

#[tokio::test]
async fn test_bidirectional() {
    let config = KcpConfig::new().fast_mode();
    let mut kcp1 = KcpEngine::new(0x55555555, config.clone());
    let mut kcp2 = KcpEngine::new(0x55555555, config);

    kcp1.start().unwrap();
    kcp2.start().unwrap();

    let message1 = b"Hello from KCP1";
    kcp1.send(Bytes::from(message1.as_slice())).unwrap();
    kcp1.flush().unwrap();

    transfer(&mut kcp1, &mut kcp2);
    kcp2.update().unwrap();

    let message2 = b"Hello back from KCP2";
    kcp2.send(Bytes::from(message2.as_slice())).unwrap();
    kcp2.flush().unwrap();

    transfer(&mut kcp2, &mut kcp1);
    kcp1.update().unwrap();

    let received1 = kcp2.recv().unwrap();
    assert!(received1.is_some());
    assert_eq!(&received1.unwrap()[..], message1);

    let received2 = kcp1.recv().unwrap();
    assert!(received2.is_some());
    assert_eq!(&received2.unwrap()[..], message2);
}

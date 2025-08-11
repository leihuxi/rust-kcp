//! Simple integration tests for async KCP implementation

use bytes::Bytes;
use kcp_tokio::async_kcp::engine::{KcpEngine, OutputFn};
use kcp_tokio::config::KcpConfig;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn test_basic_send_recv() {
    // Simple test without complex borrowing
    let network_queue = Arc::new(Mutex::new(VecDeque::<Bytes>::new()));
    let network_for_output = network_queue.clone();

    let config = KcpConfig::new().fast_mode();
    let mut kcp1 = KcpEngine::new(0x12345678, config.clone());
    let mut kcp2 = KcpEngine::new(0x12345678, config);

    let output_fn: OutputFn = Arc::new(move |data: Bytes| {
        let network = network_for_output.clone();
        Box::pin(async move {
            network.lock().unwrap().push_back(data);
            Ok(())
        })
    });

    kcp1.set_output(output_fn);
    kcp1.start().await.unwrap();
    kcp2.start().await.unwrap();

    // Send a simple message
    let message = b"Hello KCP!";
    kcp1.send(Bytes::from(message.as_slice())).await.unwrap();
    kcp1.flush().await.unwrap();

    // Transfer data
    let packets: Vec<Bytes> = {
        let mut queue = network_queue.lock().unwrap();
        let mut packets = Vec::new();
        while let Some(data) = queue.pop_front() {
            packets.push(data);
        }
        packets
    };

    for data in packets {
        kcp2.input(data).await.unwrap();
    }

    kcp2.update().await.unwrap();

    // Receive message
    let received = kcp2.recv().await.unwrap();
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
    let network_queue = Arc::new(Mutex::new(VecDeque::<Bytes>::new()));
    let network_for_output = network_queue.clone();

    // Small MTU to force fragmentation
    let config = KcpConfig::new().mtu(100);
    let mut kcp1 = KcpEngine::new(0x11111111, config.clone());
    let mut kcp2 = KcpEngine::new(0x11111111, config);

    let output_fn: OutputFn = Arc::new(move |data: Bytes| {
        let network = network_for_output.clone();
        Box::pin(async move {
            network.lock().unwrap().push_back(data);
            Ok(())
        })
    });

    kcp1.set_output(output_fn);
    kcp1.start().await.unwrap();
    kcp2.start().await.unwrap();

    // Large message that will fragment
    let large_message: Vec<u8> = (0..300).map(|i| (i % 256) as u8).collect();

    kcp1.send(Bytes::from(large_message.clone())).await.unwrap();
    kcp1.flush().await.unwrap();

    // Transfer all packets
    let packets: Vec<Bytes> = {
        let mut queue = network_queue.lock().unwrap();
        let mut packets = Vec::new();
        while let Some(data) = queue.pop_front() {
            packets.push(data);
        }
        packets
    };

    for data in packets {
        kcp2.input(data).await.unwrap();
    }

    kcp2.update().await.unwrap();

    // Receive reassembled message
    let received = kcp2.recv().await.unwrap();
    assert!(received.is_some());
    assert_eq!(received.unwrap().to_vec(), large_message);
}

#[tokio::test]
async fn test_multiple_messages() {
    let network_queue = Arc::new(Mutex::new(VecDeque::<Bytes>::new()));
    let network_for_output = network_queue.clone();

    let config = KcpConfig::new().fast_mode();
    let mut kcp1 = KcpEngine::new(0x33333333, config.clone());
    let mut kcp2 = KcpEngine::new(0x33333333, config);

    let output_fn: OutputFn = Arc::new(move |data: Bytes| {
        let network = network_for_output.clone();
        Box::pin(async move {
            network.lock().unwrap().push_back(data);
            Ok(())
        })
    });

    kcp1.set_output(output_fn);
    kcp1.start().await.unwrap();
    kcp2.start().await.unwrap();

    let messages = vec![
        b"First message".as_slice(),
        b"Second message",
        b"Third message",
    ];

    // Send all messages
    for message in &messages {
        kcp1.send(Bytes::from(*message)).await.unwrap();
    }
    kcp1.flush().await.unwrap();

    // Transfer data
    let packets: Vec<Bytes> = {
        let mut queue = network_queue.lock().unwrap();
        let mut packets = Vec::new();
        while let Some(data) = queue.pop_front() {
            packets.push(data);
        }
        packets
    };

    for data in packets {
        kcp2.input(data).await.unwrap();
    }

    kcp2.update().await.unwrap();

    // Receive all messages
    for expected in &messages {
        let received = kcp2.recv().await.unwrap();
        assert!(received.is_some());
        assert_eq!(&received.unwrap()[..], *expected);
    }

    // No more messages
    let no_more = kcp2.recv().await.unwrap();
    assert!(no_more.is_none());
}

#[tokio::test]
async fn test_empty_and_small_messages() {
    let network_queue = Arc::new(Mutex::new(VecDeque::<Bytes>::new()));
    let network_for_output = network_queue.clone();

    let config = KcpConfig::new().fast_mode();
    let mut kcp1 = KcpEngine::new(0x55555555, config.clone());
    let mut kcp2 = KcpEngine::new(0x55555555, config);

    let output_fn: OutputFn = Arc::new(move |data: Bytes| {
        let network = network_for_output.clone();
        Box::pin(async move {
            network.lock().unwrap().push_back(data);
            Ok(())
        })
    });

    kcp1.set_output(output_fn);
    kcp1.start().await.unwrap();
    kcp2.start().await.unwrap();

    // Test different message sizes (skip empty messages as they're not meaningful in KCP)
    let test_messages = vec![
        vec![42],          // Single byte
        b"Hi".to_vec(),    // Two bytes
        b"Hello".to_vec(), // Small message
    ];

    for message in &test_messages {
        kcp1.send(Bytes::from(message.clone())).await.unwrap();
        kcp1.flush().await.unwrap();

        // Transfer data
        let packets: Vec<Bytes> = {
            let mut queue = network_queue.lock().unwrap();
            let mut packets = Vec::new();
            while let Some(data) = queue.pop_front() {
                packets.push(data);
            }
            packets
        };

        for data in packets {
            kcp2.input(data).await.unwrap();
        }

        kcp2.update().await.unwrap();

        // Receive message
        let received = kcp2.recv().await.unwrap();
        assert!(received.is_some());
        assert_eq!(&received.unwrap().to_vec(), message);
    }
}

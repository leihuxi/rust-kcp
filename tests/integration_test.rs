//! Integration tests for async KCP implementation

use bytes::Bytes;
use kcp_rust::async_kcp::engine::{KcpEngine, OutputFn};
use kcp_rust::config::KcpConfig;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Shared network simulation for testing
type NetworkQueue = Arc<Mutex<VecDeque<Bytes>>>;

/// Create a test KCP engine with network queue output
async fn create_test_engine(conv: u32, network: NetworkQueue) -> KcpEngine {
    let config = KcpConfig::new().fast_mode();
    let mut engine = KcpEngine::new(conv, config);

    let output_fn: OutputFn = Arc::new(move |data: Bytes| {
        let network = network.clone();
        Box::pin(async move {
            network.lock().unwrap().push_back(data);
            Ok(())
        })
    });

    engine.set_output(output_fn);
    engine.start().await.unwrap();
    engine
}

#[tokio::test]
async fn test_async_kcp_basic_send_recv() {
    // Shared network queue to simulate UDP transport
    let network_queue = Arc::new(Mutex::new(VecDeque::<Bytes>::new()));

    // Create two KCP engines
    let mut kcp1 = create_test_engine(0x12345678, network_queue.clone()).await;
    let mut kcp2 = create_test_engine(0x12345678, network_queue.clone()).await;

    // Test message
    let message = b"Hello async KCP!";

    // Send message from kcp1
    kcp1.send(Bytes::from(message.as_slice())).await.unwrap();
    kcp1.flush().await.unwrap();

    // Transfer network data to kcp2
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

    // Update kcp2 and receive
    kcp2.update().await.unwrap();

    let received = kcp2.recv().await.unwrap();
    assert!(received.is_some());
    assert_eq!(&received.unwrap()[..], message);
}

#[tokio::test]
async fn test_async_kcp_fragmentation() {
    let network_queue = Arc::new(Mutex::new(VecDeque::<Bytes>::new()));

    // Create engines with small MTU to force fragmentation
    let config = KcpConfig::new().mtu(150); // Small MTU
    let mut kcp1 = KcpEngine::new(0x11111111, config.clone());
    let mut kcp2 = KcpEngine::new(0x11111111, config);

    let output_fn1: OutputFn = {
        let network = network_queue.clone();
        Arc::new(move |data: Bytes| {
            let network = network.clone();
            Box::pin(async move {
                network.lock().unwrap().push_back(data);
                Ok(())
            })
        })
    };

    kcp1.set_output(output_fn1);
    kcp1.start().await.unwrap();
    kcp2.start().await.unwrap();

    // Large message that will be fragmented
    let large_message: Vec<u8> = (0..500).map(|i| (i % 256) as u8).collect();

    // Send large message
    kcp1.send(Bytes::from(large_message.clone())).await.unwrap();
    kcp1.flush().await.unwrap();

    // Transfer all network data
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
async fn test_async_kcp_multiple_messages() {
    let network_queue = Arc::new(Mutex::new(VecDeque::<Bytes>::new()));

    let mut kcp1 = create_test_engine(0x33333333, network_queue.clone()).await;
    let mut kcp2 = create_test_engine(0x33333333, network_queue.clone()).await;

    let messages = vec![
        b"First message".as_slice(),
        b"Second message",
        b"Third message",
    ];

    // Send multiple messages
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
    let network_queue1to2 = Arc::new(Mutex::new(VecDeque::<Bytes>::new()));
    let network_queue2to1 = Arc::new(Mutex::new(VecDeque::<Bytes>::new()));

    // Create engines with separate network paths
    let config = KcpConfig::new().fast_mode();
    let mut kcp1 = KcpEngine::new(0x55555555, config.clone());
    let mut kcp2 = KcpEngine::new(0x55555555, config);

    // Set output functions for bidirectional communication
    let output_fn1: OutputFn = {
        let network = network_queue1to2.clone();
        Arc::new(move |data: Bytes| {
            let network = network.clone();
            Box::pin(async move {
                network.lock().unwrap().push_back(data);
                Ok(())
            })
        })
    };

    let output_fn2: OutputFn = {
        let network = network_queue2to1.clone();
        Arc::new(move |data: Bytes| {
            let network = network.clone();
            Box::pin(async move {
                network.lock().unwrap().push_back(data);
                Ok(())
            })
        })
    };

    kcp1.set_output(output_fn1);
    kcp2.set_output(output_fn2);
    kcp1.start().await.unwrap();
    kcp2.start().await.unwrap();

    // Send from kcp1 to kcp2
    let message1 = b"Hello from KCP1";
    kcp1.send(Bytes::from(message1.as_slice())).await.unwrap();
    kcp1.flush().await.unwrap();

    // Transfer 1->2
    let packets: Vec<Bytes> = {
        let mut queue = network_queue1to2.lock().unwrap();
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

    // Send response from kcp2 to kcp1
    let message2 = b"Hello back from KCP2";
    kcp2.send(Bytes::from(message2.as_slice())).await.unwrap();
    kcp2.flush().await.unwrap();

    // Transfer 2->1
    let packets: Vec<Bytes> = {
        let mut queue = network_queue2to1.lock().unwrap();
        let mut packets = Vec::new();
        while let Some(data) = queue.pop_front() {
            packets.push(data);
        }
        packets
    };

    for data in packets {
        kcp1.input(data).await.unwrap();
    }
    kcp1.update().await.unwrap();

    // Verify both received correctly
    let received1 = kcp2.recv().await.unwrap();
    assert!(received1.is_some());
    assert_eq!(&received1.unwrap()[..], message1);

    let received2 = kcp1.recv().await.unwrap();
    assert!(received2.is_some());
    assert_eq!(&received2.unwrap()[..], message2);
}

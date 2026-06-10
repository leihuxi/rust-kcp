//! Regression tests for close semantics, dead-peer detection, config
//! validation, and IPv6 support.

use kcp_tokio::config::KcpConfig;
use kcp_tokio::{KcpListener, KcpStream};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::time::timeout;

/// `shutdown().await` followed by drop must not lose the unacknowledged tail,
/// even under packet loss: poll_shutdown waits for the actor's graceful drain
/// and Drop no longer aborts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shutdown_then_drop_delivers_tail_under_loss() {
    const PAYLOAD: usize = 64 * 1024;

    let server_config = KcpConfig::new().fast_mode().window_size(128, 128);
    let (addr_tx, addr_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut listener = KcpListener::bind("127.0.0.1:0".parse().unwrap(), server_config)
            .await
            .unwrap();
        addr_tx.send(*listener.local_addr()).unwrap();
        let (mut stream, _) = listener.accept().await.unwrap();

        let mut received = Vec::with_capacity(PAYLOAD);
        let mut buf = [0u8; 65536];
        while received.len() < PAYLOAD {
            match timeout(Duration::from_secs(15), stream.read(&mut buf)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => received.extend_from_slice(&buf[..n]),
                _ => break,
            }
        }
        received
    });
    let addr = addr_rx.await.unwrap();

    // 20% outbound loss forces the tail to survive via retransmission during
    // the post-shutdown drain.
    let client_config = KcpConfig::new()
        .fast_mode()
        .window_size(128, 128)
        .simulate_packet_loss(0.2);
    let mut client = KcpStream::connect(addr, client_config).await.unwrap();

    let data: Vec<u8> = (0..PAYLOAD).map(|i| (i % 251) as u8).collect();
    client.write_all(&data).await.unwrap();
    client.shutdown().await.unwrap();
    drop(client);

    let received = server.await.unwrap();
    assert_eq!(received.len(), PAYLOAD, "tail lost on shutdown + drop");
    assert_eq!(received, data, "data corrupted");
}

/// A peer that vanishes without closing (no FIN exists in KCP) must be
/// detected by the keep-alive idle check; the server stream reaches EOF
/// instead of leaking forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_dead_peer_detected_by_keep_alive() {
    let server_config = KcpConfig::new()
        .fast_mode()
        .keep_alive(Some(Duration::from_millis(200)));
    let (addr_tx, addr_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut listener = KcpListener::bind("127.0.0.1:0".parse().unwrap(), server_config)
            .await
            .unwrap();
        addr_tx.send(*listener.local_addr()).unwrap();
        let (mut stream, _) = listener.accept().await.unwrap();

        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).await.unwrap();
        assert!(n > 0);

        // Peer is gone; expect EOF (or error) within a few keep-alive windows.
        match timeout(Duration::from_secs(5), stream.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) => (),
            Ok(Ok(_)) => panic!("unexpected data from vanished peer"),
            Err(_) => panic!("dead peer not detected within 5s"),
        }
    });
    let addr = addr_rx.await.unwrap();

    let mut client = KcpStream::connect(addr, KcpConfig::new().fast_mode())
        .await
        .unwrap();
    client.write_all(b"hello").await.unwrap();
    client.flush().await.unwrap();
    // Give the server a moment to read, then vanish abruptly.
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(client);

    server.await.unwrap();
}

/// Echo over IPv6 — `connect` must bind in the target's address family.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_ipv6_echo() {
    let config = KcpConfig::new().fast_mode();
    let mut listener = match KcpListener::bind("[::1]:0".parse().unwrap(), config.clone()).await {
        Ok(l) => l,
        Err(_) => return, // environment without IPv6 support
    };
    let addr = *listener.local_addr();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 64];
        let n = stream.read(&mut buf).await.unwrap();
        stream.write_all(&buf[..n]).await.unwrap();
        stream.flush().await.unwrap();
        let _ = stream.close().await;
    });

    let mut client = KcpStream::connect(addr, config).await.unwrap();
    client.write_all(b"v6 echo").await.unwrap();
    client.flush().await.unwrap();

    let mut buf = [0u8; 64];
    let n = timeout(Duration::from_secs(5), client.read(&mut buf))
        .await
        .expect("timeout waiting for v6 echo")
        .unwrap();
    assert_eq!(&buf[..n], b"v6 echo");

    let _ = client.close().await;
    let _ = server.await;
}

/// The main APIs (`KcpStream::connect`, `KcpListener::bind`) must validate the
/// config, not just the convenience `KcpConfig::connect/listen` wrappers.
#[tokio::test]
async fn test_invalid_config_rejected_by_main_apis() {
    let bad = KcpConfig::new().mtu(0);
    assert!(
        KcpStream::connect("127.0.0.1:9".parse().unwrap(), bad.clone())
            .await
            .is_err()
    );
    assert!(
        KcpListener::bind("127.0.0.1:0".parse().unwrap(), bad)
            .await
            .is_err()
    );
}

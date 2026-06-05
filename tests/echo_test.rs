//! Integration test for KCP echo server/client

use kcp_tokio::{KcpListener, KcpStream};
use kcp_tokio::config::KcpConfig;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::time::timeout;

/// Spawn an echo server on an OS-assigned port and return the actual address.
async fn spawn_echo_server(
    config: KcpConfig,
    message_count: usize,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let (addr_tx, addr_rx) = oneshot::channel();

    let handle = tokio::spawn(async move {
        let mut listener = KcpListener::bind("127.0.0.1:0".parse().unwrap(), config)
            .await
            .expect("Failed to bind listener");

        // Report actual address back
        addr_tx.send(*listener.local_addr()).unwrap();

        let (mut stream, _peer_addr) = listener.accept().await.expect("Failed to accept");

        let mut buf = [0u8; 65536];
        for _ in 0..message_count {
            let n = stream.read(&mut buf).await.expect("Failed to read");
            stream.write_all(&buf[..n]).await.expect("Failed to write");
            stream.flush().await.expect("Failed to flush");
        }

        // Explicit cleanup
        let _ = stream.close().await;
        let _ = listener.close().await;
    });

    let addr = addr_rx.await.unwrap();
    (addr, handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_echo_server_client() {
    let server_config = KcpConfig::realtime();
    let (server_addr, server_handle) = spawn_echo_server(server_config, 1).await;

    let client_config = KcpConfig::new().fast_mode().window_size(128, 128).mtu(1400);
    let mut client = KcpStream::connect(server_addr, client_config)
        .await
        .expect("Failed to connect");

    let test_msg = b"Hello, KCP!";
    client.write_all(test_msg).await.expect("Failed to send");
    client.flush().await.expect("Failed to flush");

    let mut buf = [0u8; 1024];
    let n = timeout(Duration::from_secs(5), client.read(&mut buf))
        .await
        .expect("Timeout waiting for echo")
        .expect("Failed to read");

    assert_eq!(&buf[..n], test_msg, "Echo response mismatch");

    let _ = client.close().await;
    let _ = timeout(Duration::from_secs(1), server_handle).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_large_transfer_backpressure() {
    // Stream far more data than any single window through small windows so the
    // send path must apply backpressure many times over. Verifies the bounded
    // send queue neither drops nor reorders bytes — i.e. backpressure preserves
    // reliability under a fast writer over a slow link.
    const TOTAL: usize = 256 * 1024;

    let (addr_tx, addr_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let config = KcpConfig::new().fast_mode().window_size(16, 16).mtu(1200);
        let mut listener = KcpListener::bind("127.0.0.1:0".parse().unwrap(), config)
            .await
            .unwrap();
        addr_tx.send(*listener.local_addr()).unwrap();
        let (mut stream, _) = listener.accept().await.unwrap();

        let mut buf = vec![0u8; 65536];
        let mut echoed = 0usize;
        while echoed < TOTAL {
            let n = stream.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            stream.write_all(&buf[..n]).await.unwrap();
            echoed += n;
        }
        stream.flush().await.unwrap();
        let _ = stream.close().await;
        let _ = listener.close().await;
    });

    let addr = addr_rx.await.unwrap();
    let client_config = KcpConfig::new().fast_mode().window_size(16, 16).mtu(1200);
    let client = KcpStream::connect(addr, client_config).await.unwrap();

    let payload: Vec<u8> = (0..TOTAL).map(|i| (i % 251) as u8).collect();
    let payload_w = payload.clone();

    // Split so the client can write and read its echo concurrently — a lockstep
    // writer would never trigger sustained backpressure.
    let (mut rd, mut wr) = tokio::io::split(client);
    let writer = tokio::spawn(async move {
        wr.write_all(&payload_w).await.unwrap();
        wr.flush().await.unwrap();
    });

    let mut got = vec![0u8; TOTAL];
    timeout(Duration::from_secs(30), rd.read_exact(&mut got))
        .await
        .expect("timeout receiving echo under backpressure")
        .expect("read_exact failed");

    writer.await.unwrap();
    assert_eq!(got, payload, "echoed data mismatch under backpressure");

    let _ = timeout(Duration::from_secs(2), server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_multiple_messages() {
    let server_config = KcpConfig::realtime();
    let (server_addr, server_handle) = spawn_echo_server(server_config, 3).await;

    let client_config = KcpConfig::new().fast_mode().window_size(128, 128).mtu(1400);
    let mut client = KcpStream::connect(server_addr, client_config)
        .await
        .expect("Failed to connect");

    let messages = ["First message", "Second message", "Third message"];

    for msg in &messages {
        client
            .write_all(msg.as_bytes())
            .await
            .expect("Failed to send");
        client.flush().await.expect("Failed to flush");

        let mut buf = [0u8; 1024];
        let n = timeout(Duration::from_secs(2), client.read(&mut buf))
            .await
            .expect("Timeout")
            .expect("Failed to read");

        assert_eq!(&buf[..n], msg.as_bytes(), "Echo mismatch for '{}'", msg);
    }

    let _ = client.close().await;
    let _ = timeout(Duration::from_secs(1), server_handle).await;
}

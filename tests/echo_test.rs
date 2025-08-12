//! Integration test for KCP echo server/client

use kcp_tokio::async_kcp::{KcpListener, KcpStream};
use kcp_tokio::config::KcpConfig;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

#[tokio::test]
async fn test_echo_server_client() {
    // Start server
    let server_addr = "127.0.0.1:18888";
    let server_config = KcpConfig::realtime();

    let server_handle = tokio::spawn(async move {
        let mut listener = KcpListener::bind(server_addr.parse().unwrap(), server_config)
            .await
            .expect("Failed to bind listener");

        // Accept one connection
        let (mut stream, peer_addr) = listener.accept().await.expect("Failed to accept");
        println!("Server: Accepted connection from {}", peer_addr);

        // Read and echo back
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).await.expect("Failed to read");
        let msg = String::from_utf8_lossy(&buf[..n]);
        println!("Server: Received '{}'", msg);

        stream.write_all(&buf[..n]).await.expect("Failed to write");
        stream.flush().await.expect("Failed to flush");
        println!("Server: Echoed back '{}'", msg);
    });

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Connect client
    let client_config = KcpConfig::new().fast_mode().window_size(128, 128).mtu(1400);
    let mut client = KcpStream::connect(server_addr.parse().unwrap(), client_config)
        .await
        .expect("Failed to connect");

    println!("Client: Connected to server");

    // Send message
    let test_msg = b"Hello, KCP!";
    client.write_all(test_msg).await.expect("Failed to send");
    client.flush().await.expect("Failed to flush");
    println!("Client: Sent '{}'", String::from_utf8_lossy(test_msg));

    // Receive echo with timeout
    let mut buf = [0u8; 1024];
    let result = timeout(Duration::from_secs(5), client.read(&mut buf)).await;

    match result {
        Ok(Ok(n)) => {
            let response = &buf[..n];
            println!(
                "Client: Received echo '{}'",
                String::from_utf8_lossy(response)
            );
            assert_eq!(response, test_msg, "Echo response doesn't match");
            println!("✅ Test passed: Message successfully echoed");
        }
        Ok(Err(e)) => panic!("Failed to read echo: {}", e),
        Err(_) => panic!("Timeout waiting for echo response"),
    }

    // Wait for server to finish
    let _ = timeout(Duration::from_secs(1), server_handle).await;
}

#[tokio::test]
async fn test_multiple_messages() {
    let server_addr = "127.0.0.1:18889";
    let server_config = KcpConfig::realtime();

    let server_handle = tokio::spawn(async move {
        let mut listener = KcpListener::bind(server_addr.parse().unwrap(), server_config)
            .await
            .expect("Failed to bind listener");

        let (mut stream, _) = listener.accept().await.expect("Failed to accept");

        // Echo multiple messages
        let mut buf = [0u8; 1024];
        for i in 0..3 {
            let n = stream.read(&mut buf).await.expect("Failed to read");
            println!(
                "Server: Message {}: '{}'",
                i + 1,
                String::from_utf8_lossy(&buf[..n])
            );
            stream.write_all(&buf[..n]).await.expect("Failed to write");
            stream.flush().await.expect("Failed to flush");
        }
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let client_config = KcpConfig::new().fast_mode().window_size(128, 128).mtu(1400);
    let mut client = KcpStream::connect(server_addr.parse().unwrap(), client_config)
        .await
        .expect("Failed to connect");

    let messages = vec!["First message", "Second message", "Third message"];

    for msg in &messages {
        // Send
        client
            .write_all(msg.as_bytes())
            .await
            .expect("Failed to send");
        client.flush().await.expect("Failed to flush");

        // Receive echo
        let mut buf = [0u8; 1024];
        let n = timeout(Duration::from_secs(2), client.read(&mut buf))
            .await
            .expect("Timeout")
            .expect("Failed to read");

        let response = String::from_utf8_lossy(&buf[..n]);
        assert_eq!(response, *msg, "Echo mismatch");
        println!("✅ Message '{}' echoed correctly", msg);
    }

    println!("✅ All messages echoed successfully");
    let _ = timeout(Duration::from_secs(1), server_handle).await;
}

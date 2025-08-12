use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;

#[tokio::test]
async fn test_exact_kcp_socket_pattern() {
    // This test replicates the EXACT socket setup pattern used by KCP

    let server_addr = "127.0.0.1:18895";

    // Server side: Mirror KCP listener socket setup
    println!("Setting up server socket like KCP listener...");
    let server_socket = UdpSocket::bind(server_addr)
        .await
        .expect("Failed to bind server");
    let server_socket = Arc::new(server_socket);
    println!("Server bound to {}", server_socket.local_addr().unwrap());

    // Start server receive task
    let server_socket_clone = server_socket.clone();
    let server_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 65536]; // Same buffer size as KCP listener

        println!("Server: Starting recv_from loop...");
        for i in 0..5 {
            // Try receiving a few packets
            match tokio::time::timeout(
                Duration::from_millis(500),
                server_socket_clone.recv_from(&mut buf),
            )
            .await
            {
                Ok(Ok((size, peer_addr))) => {
                    println!(
                        "Server: [{}] Received {} bytes from {}: {:02x?}",
                        i,
                        size,
                        peer_addr,
                        &buf[..size.min(16)]
                    );
                }
                Ok(Err(e)) => {
                    println!("Server: [{}] recv_from error: {}", i, e);
                }
                Err(_) => {
                    println!("Server: [{}] recv_from timeout", i);
                }
            }
        }
    });

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Client side: Mirror KCP client socket setup
    println!("Setting up client socket like KCP client...");
    let client_socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .expect("Failed to bind client");
    client_socket
        .connect(server_addr)
        .await
        .expect("Failed to connect client");
    println!(
        "Client bound to {} and connected to {}",
        client_socket.local_addr().unwrap(),
        server_addr
    );

    // Send packets like KCP client does
    for i in 0..3 {
        let test_data = format!("Test packet {}", i);
        println!("Client: Sending packet {}: '{}'", i, test_data);

        match client_socket.send(test_data.as_bytes()).await {
            Ok(size) => {
                println!("Client: Sent {} bytes successfully", size);
            }
            Err(e) => {
                println!("Client: Failed to send: {}", e);
            }
        }

        // Small delay between packets
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Wait for server task
    let _ = server_task.await;

    println!("Socket pattern test completed");
}

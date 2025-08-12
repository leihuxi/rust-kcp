use std::time::Duration;
use tokio::net::UdpSocket;

#[tokio::test]
async fn test_basic_udp_debug() {
    // Server
    let server_addr = "127.0.0.1:18891";
    let server = UdpSocket::bind(server_addr)
        .await
        .expect("Failed to bind server");
    println!("Server bound to {}", server.local_addr().unwrap());

    // Client
    let client = UdpSocket::bind("0.0.0.0:0")
        .await
        .expect("Failed to bind client");
    client
        .connect(server_addr)
        .await
        .expect("Failed to connect client");
    println!(
        "Client bound to {} and connected to server",
        client.local_addr().unwrap()
    );

    // Spawn server task
    let server_task = tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        println!("Server: Waiting for packets...");

        match server.recv_from(&mut buf).await {
            Ok((size, peer_addr)) => {
                let msg = std::str::from_utf8(&buf[..size]).unwrap_or("<invalid>");
                println!("Server: Received '{}' from {}", msg, peer_addr);

                // Echo back
                if let Err(e) = server.send_to(b"Echo received", peer_addr).await {
                    println!("Server: Failed to send echo: {}", e);
                } else {
                    println!("Server: Sent echo back to {}", peer_addr);
                }
            }
            Err(e) => {
                println!("Server: Failed to receive: {}", e);
            }
        }
    });

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send from client to server
    println!("Client: Sending message...");
    match client.send(b"Hello from client").await {
        Ok(size) => {
            println!("Client: Sent {} bytes", size);
        }
        Err(e) => {
            println!("Client: Failed to send: {}", e);
        }
    }

    // Try to receive response
    println!("Client: Waiting for response...");
    let mut buf = [0u8; 1024];
    match tokio::time::timeout(Duration::from_secs(2), client.recv(&mut buf)).await {
        Ok(Ok(size)) => {
            let response = std::str::from_utf8(&buf[..size]).unwrap_or("<invalid>");
            println!("Client: Received response: '{}'", response);
        }
        Ok(Err(e)) => {
            println!("Client: Error receiving: {}", e);
        }
        Err(_) => {
            println!("Client: Timeout waiting for response");
        }
    }

    // Wait for server task
    let _ = tokio::time::timeout(Duration::from_secs(1), server_task).await;
    println!("Basic UDP test completed");
}

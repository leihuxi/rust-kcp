use kcp_tokio::async_kcp::{KcpListener, KcpStream};
use kcp_tokio::config::KcpConfig;
use std::time::Duration;
use tracing::info;

#[tokio::test]
async fn test_echo_timing_issue() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .init();

    let server_addr = "127.0.0.1:18898";

    // Start server in background (like in echo test)
    let server_task = tokio::spawn(async move {
        let server_config = KcpConfig::realtime();
        let mut listener = KcpListener::bind(server_addr.parse().unwrap(), server_config)
            .await
            .expect("Failed to bind listener");

        info!("Server: Waiting to accept connection...");

        // Accept one connection (this might be the problem - timing)
        let (mut stream, peer_addr) = listener.accept().await.expect("Failed to accept");
        info!("Server: Accepted connection from {}", peer_addr);

        // Try to read data
        match stream.recv().await {
            Ok(Some(data)) => {
                let msg = String::from_utf8_lossy(&data);
                info!("Server: Received message: '{}'", msg);

                // Echo it back
                let echo_msg = format!("Echo: {}", msg);
                match stream.send(echo_msg.as_bytes()).await {
                    Ok(_) => {
                        info!("Server: Sent echo response: '{}'", echo_msg);
                    }
                    Err(e) => {
                        info!("Server: Failed to send echo: {}", e);
                    }
                }
            }
            Ok(None) => {
                info!("Server: No data received");
            }
            Err(e) => {
                info!("Server: Failed to receive: {}", e);
            }
        }

        // Keep server alive a bit longer
        tokio::time::sleep(Duration::from_secs(1)).await;
        info!("Server: Task ending");
    });

    // Give server time to start and call accept()
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create client
    info!("Client: Creating connection...");
    let client_config = KcpConfig::new().fast_mode().window_size(128, 128).mtu(1400);
    let mut client = KcpStream::connect(server_addr.parse().unwrap(), client_config)
        .await
        .expect("Failed to connect");

    info!("Client: Connected successfully");

    // Give a bit more time for the server to complete accept()
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Send message
    let test_msg = "Hello World";
    info!("Client: Sending message: '{}'", test_msg);
    match client.send(test_msg.as_bytes()).await {
        Ok(_) => {
            info!("Client: Message sent successfully");
        }
        Err(e) => {
            info!("Client: Failed to send message: {}", e);
        }
    }

    // Try to receive response
    info!("Client: Waiting for response...");
    match tokio::time::timeout(Duration::from_secs(3), client.recv()).await {
        Ok(Ok(Some(data))) => {
            let response = String::from_utf8_lossy(&data);
            info!("Client: Received response: '{}'", response);
            assert!(
                response.contains("Hello World"),
                "Response should contain original message"
            );
            info!("✅ SUCCESS: Echo communication works!");
        }
        Ok(Ok(None)) => {
            info!("❌ Client: No data received");
        }
        Ok(Err(e)) => {
            info!("❌ Client: Error receiving: {}", e);
        }
        Err(_) => {
            info!("❌ Client: Timeout waiting for response");
        }
    }

    // Wait for server task
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
    info!("Test completed");
}

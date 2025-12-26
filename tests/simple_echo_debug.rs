use kcp_tokio::async_kcp::{KcpListener, KcpStream};
use kcp_tokio::config::KcpConfig;
use std::time::Duration;
use tokio::time::timeout;
use tracing::info;

#[tokio::test]
async fn test_debug_echo() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .init();

    let server_addr = "127.0.0.1:18890";

    // Server task
    let server_handle = tokio::spawn(async move {
        let server_config = KcpConfig::realtime();
        let mut listener = KcpListener::bind(server_addr.parse().unwrap(), server_config)
            .await
            .expect("Failed to bind listener");

        info!("Server: Listening on {}", listener.local_addr());

        // Accept connection
        let (mut stream, peer_addr) = listener.accept().await.expect("Failed to accept");
        info!("Server: Accepted connection from {}", peer_addr);

        // Read message
        match stream.recv().await {
            Ok(Some(data)) => {
                let msg = String::from_utf8_lossy(&data);
                info!("Server: Received message: '{}'", msg);

                // Send echo back
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

        // Keep server alive for a bit
        tokio::time::sleep(Duration::from_secs(2)).await;
    });

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Client
    let client_config = KcpConfig::new().fast_mode().window_size(128, 128).mtu(1400);
    let mut client = KcpStream::connect(server_addr.parse().unwrap(), client_config)
        .await
        .expect("Failed to connect");

    info!("Client: Connected to server");

    // Give the client time to establish connection (send initial packets)
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send message
    let test_msg = "Hello";
    match client.send(test_msg.as_bytes()).await {
        Ok(_) => {
            info!("Client: Sent message: '{}'", test_msg);
        }
        Err(e) => {
            info!("Client: Failed to send: {}", e);
        }
    }

    // Try to receive with timeout
    info!("Client: Waiting for echo response...");
    let result = timeout(Duration::from_secs(3), client.recv()).await;

    match result {
        Ok(Ok(Some(data))) => {
            let response = String::from_utf8_lossy(&data);
            info!("Client: Received response: '{}'", response);
            assert!(
                response.contains("Hello"),
                "Response should contain 'Hello'"
            );
        }
        Ok(Ok(None)) => {
            panic!("Client: No data received");
        }
        Ok(Err(e)) => {
            panic!("Client: Error receiving: {}", e);
        }
        Err(_) => {
            panic!("Client: Timeout waiting for response");
        }
    }

    // Wait for server to finish
    let _ = timeout(Duration::from_secs(1), server_handle).await;
    info!("Test completed");
}

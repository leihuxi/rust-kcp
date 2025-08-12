use kcp_tokio::async_kcp::{KcpListener, KcpStream};
use kcp_tokio::config::KcpConfig;
use std::time::Duration;
use tracing::info;

#[tokio::test]
async fn test_handshake_packet_reception() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .init();

    let server_addr = "127.0.0.1:18894";
    
    // Start server in background that just waits for packets (doesn't call accept)
    let server_task = tokio::spawn(async move {
        let listener = KcpListener::bind(server_addr.parse().unwrap(), KcpConfig::realtime())
            .await
            .expect("Failed to bind listener");
        
        info!("Server: Listener bound at {}", listener.local_addr());
        
        // Just wait and let the background task receive packets
        tokio::time::sleep(Duration::from_secs(3)).await;
        info!("Server: Done waiting");
    });
    
    // Give server time to start
    tokio::time::sleep(Duration::from_millis(300)).await;
    
    // Create client (this should trigger handshake packets)
    info!("Creating KCP client to trigger handshake...");
    let client_config = KcpConfig::new().fast_mode().window_size(128, 128).mtu(1400);
    let _client = KcpStream::connect(server_addr.parse().unwrap(), client_config)
        .await;
    
    match _client {
        Ok(_) => info!("Client: Connected successfully"),
        Err(e) => info!("Client: Connection failed: {}", e),
    }
    
    // Wait for server task to complete
    let _ = server_task.await;
    
    info!("Test completed - check logs for packet reception during handshake");
}
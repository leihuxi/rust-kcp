use kcp_tokio::async_kcp::{KcpListener, KcpStream};
use kcp_tokio::config::KcpConfig;
use std::time::Duration;
use tracing::info;

#[tokio::test]
async fn test_listener_without_accept() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .init();

    let server_addr = "127.0.0.1:18897";
    
    // Create listener but DON'T call accept - just let it run
    info!("Creating KCP listener without calling accept...");
    let _listener = KcpListener::bind(server_addr.parse().unwrap(), KcpConfig::realtime())
        .await
        .expect("Failed to bind listener");
    
    // Give listener time to start background task
    tokio::time::sleep(Duration::from_millis(300)).await;
    
    // Create KCP client and send data
    info!("Creating KCP client...");
    let client_config = KcpConfig::new().fast_mode().window_size(128, 128).mtu(1400);
    let mut client = KcpStream::connect(server_addr.parse().unwrap(), client_config)
        .await
        .expect("Failed to connect");
    
    info!("Client connected, now sending data...");
    
    // Send data (this should trigger KCP packets)
    match client.send(b"Hello World").await {
        Ok(_) => info!("Client sent data successfully"),
        Err(e) => info!("Client failed to send data: {}", e),
    }
    
    // Give listener time to process packets
    info!("Waiting for listener to process packets...");
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    info!("Test completed - check logs for packet reception");
}
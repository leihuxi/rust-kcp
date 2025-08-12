use kcp_tokio::async_kcp::KcpListener;
use kcp_tokio::config::KcpConfig;
use std::time::Duration;
use tokio::net::UdpSocket;
use tracing::info;

#[tokio::test]
async fn test_listener_packet_reception() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .init();

    let server_addr = "127.0.0.1:18893";

    // Create KCP listener but DON'T call accept() yet
    info!("Creating KCP listener...");
    let listener = KcpListener::bind(server_addr.parse().unwrap(), KcpConfig::realtime())
        .await
        .expect("Failed to bind listener");

    info!("KCP listener created at {}", listener.local_addr());

    // Give listener time to start background task
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Create client and send packets
    info!("Creating client...");
    let client = UdpSocket::bind("0.0.0.0:0")
        .await
        .expect("Failed to bind client");
    client
        .connect(server_addr)
        .await
        .expect("Failed to connect client");

    info!("Client connected from {}", client.local_addr().unwrap());

    // Send some raw UDP data (not necessarily valid KCP)
    info!("Sending test packet...");
    let test_data = b"Hello from test client";
    match client.send(test_data).await {
        Ok(size) => info!("Client sent {} bytes", size),
        Err(e) => info!("Client failed to send: {}", e),
    }

    // Wait a bit to see if the listener receives it
    info!("Waiting for listener to process packet...");
    tokio::time::sleep(Duration::from_secs(2)).await;

    info!("Test completed - check above logs for packet reception");

    // Don't call accept() - just let the listener exist to see if it receives packets
}

//! Quick test for Rust KCP communication with C server

use bytes::Bytes;
use kcp_tokio::async_kcp::engine::KcpEngine;
use kcp_tokio::config::KcpConfig;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let server_addr: SocketAddr = "127.0.0.1:8888".parse()?;

    // Create UDP socket
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(server_addr).await?;
    let socket = Arc::new(socket);

    // Create KCP engine
    let conv = 0x12345678u32;
    let config = KcpConfig::new().fast_mode();
    let mut engine = KcpEngine::new(conv, config);
    engine.start()?;

    info!("Quick test started, sending one message...");

    // Send a test message
    engine.send(Bytes::from("Quick test!"))?;
    info!("Message sent to KCP engine");

    // Flush output to the network
    for buf in engine.drain_output() {
        socket.send(&buf).await?;
    }

    // Simple receive loop
    let mut recv_buf = vec![0u8; 2048];
    for _ in 0..50 {
        // Try to receive a UDP packet with a short timeout
        if let Ok(Ok(size)) = tokio::time::timeout(
            tokio::time::Duration::from_millis(100),
            socket.recv(&mut recv_buf),
        )
        .await
        {
            info!("Received UDP packet: {} bytes", size);
            let data = Bytes::copy_from_slice(&recv_buf[..size]);
            if let Err(e) = engine.input(data) {
                error!("KCP input error: {}", e);
            }

            // Try to receive KCP data
            if let Ok(Some(kcp_data)) = engine.recv() {
                let response = String::from_utf8_lossy(&kcp_data);
                info!("✓ Got KCP response: {}", response);
                break;
            }
        }

        // Periodic update
        let _ = engine.update();
        for buf in engine.drain_output() {
            let _ = socket.send(&buf).await;
        }
    }

    info!("Quick test completed");
    Ok(())
}

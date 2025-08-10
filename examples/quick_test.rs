//! Quick test for Rust KCP communication with C server

use bytes::Bytes;
use kcp_rust::async_kcp::engine::{KcpEngine, OutputFn};
use kcp_rust::config::KcpConfig;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
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

    // Set output function
    let socket_clone = socket.clone();
    let output_fn: OutputFn = Arc::new(move |data: Bytes| {
        let socket = socket_clone.clone();
        Box::pin(async move {
            socket
                .send(&data)
                .await
                .map_err(kcp_rust::error::KcpError::Io)?;
            info!("Sent {} bytes to C server", data.len());
            Ok(())
        })
    });

    engine.set_output(output_fn);
    engine.start().await?;
    let engine = Arc::new(Mutex::new(engine));

    info!("Quick test started, sending one message...");

    // Send a test message
    {
        let mut engine = engine.lock().await;
        engine.send(Bytes::from("Quick test!")).await?;
        info!("Message sent to KCP engine");
    }

    // Simple receive loop
    tokio::spawn({
        let engine = engine.clone();
        let socket = socket.clone();
        async move {
            let mut buf = vec![0u8; 2048];
            for _ in 0..10 {
                if let Ok(size) = socket.recv(&mut buf).await {
                    info!("Received UDP packet: {} bytes", size);
                    let data = Bytes::copy_from_slice(&buf[..size]);
                    let mut engine = engine.lock().await;
                    if let Err(e) = engine.input(data).await {
                        error!("KCP input error: {}", e);
                    }

                    // Try to receive KCP data
                    if let Ok(Some(kcp_data)) = engine.recv().await {
                        let response = String::from_utf8_lossy(&kcp_data);
                        info!("✓ Got KCP response: {}", response);
                        return;
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
        }
    });

    // Update loop
    for _ in 0..50 {
        {
            let mut engine = engine.lock().await;
            engine.update().await?;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    info!("Quick test completed");
    Ok(())
}

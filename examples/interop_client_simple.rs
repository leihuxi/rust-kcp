//! Simplified KCP Rust client interoperability test with C server
//! No handshake mechanism, direct communication through KCP protocol

use bytes::Bytes;
use kcp_tokio::async_kcp::engine::{KcpEngine, OutputFn};
use kcp_tokio::config::KcpConfig;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    let server_addr: SocketAddr = "127.0.0.1:8888".parse()?;

    info!("=== KCP Rust-C Simplified Interoperability Test ===");
    info!("Connecting to C server: {}", server_addr);

    // Create UDP socket
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(server_addr).await?;
    let socket = Arc::new(socket);

    info!("UDP connection established");

    // Create KCP engine - use same session ID as C server
    let conv = 0x12345678u32;
    let config = KcpConfig::new()
        .fast_mode() // nodelay=1, interval=10, resend=2, nc=1
        .window_size(128, 128)
        .mtu(1400);

    let mut engine = KcpEngine::new(conv, config);

    // Set output function
    let socket_clone = socket.clone();
    let output_fn: OutputFn = Arc::new(move |data: Bytes| {
        let socket = socket_clone.clone();
        Box::pin(async move {
            socket
                .send(&data)
                .await
                .map_err(kcp_tokio::error::KcpError::Io)?;
            Ok(())
        })
    });

    engine.set_output(output_fn);
    engine.start().await?;

    let engine = Arc::new(Mutex::new(engine));

    info!("KCP engine started (conv=0x{:08x})", conv);

    // Start receive task
    let engine_recv = engine.clone();
    let socket_recv = socket.clone();
    let recv_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        loop {
            match socket_recv.recv(&mut buf).await {
                Ok(size) => {
                    let data = Bytes::copy_from_slice(&buf[..size]);
                    info!("Received UDP packet: {} bytes", size);

                    let mut engine = engine_recv.lock().await;
                    if let Err(e) = engine.input(data).await {
                        error!("KCP input error: {}", e);
                    }
                }
                Err(e) => {
                    error!("UDP receive error: {}", e);
                    break;
                }
            }
        }
    });

    // Start update task
    let engine_update = engine.clone();
    let update_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(10));
        loop {
            interval.tick().await;
            let mut engine = engine_update.lock().await;
            if let Err(e) = engine.update().await {
                error!("KCP update error: {}", e);
            }
        }
    });

    // Test messages
    let test_messages = [
        "Hello from Rust!",
        "KCP interoperability test",
        "Message #3",
        "Testing UTF-8",
        "Final message",
    ];

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    for (i, message) in test_messages.iter().enumerate() {
        info!("Sending message {}: {}", i + 1, message);

        // Send message
        {
            let mut engine = engine.lock().await;
            match engine.send(Bytes::from(message.as_bytes())).await {
                Ok(()) => {
                    info!("✓ Message sent to KCP");
                }
                Err(e) => {
                    error!("✗ Send failed: {}", e);
                    continue;
                }
            }
        }

        // Wait for response
        let mut attempts = 0;
        let max_attempts = 50; // 5 second timeout (50 * 100ms)

        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            attempts += 1;

            let recv_result = {
                let mut engine = engine.lock().await;
                engine.recv().await
            };

            match recv_result {
                Ok(Some(data)) => {
                    let response = String::from_utf8_lossy(&data);
                    info!("✓ Received C server response: {}", response);

                    // Verify response format
                    if let Some(echoed_part) = response.strip_prefix("C_ECHO: ") {
                        if echoed_part == *message {
                            info!("✓ Response verification successful");
                        } else {
                            warn!(
                                "⚠ Response content mismatch: expected '{}', got '{}'",
                                message, echoed_part
                            );
                        }
                    } else {
                        warn!("⚠ Response format mismatch: {}", response);
                    }
                    break;
                }
                Ok(None) => {
                    // Continue waiting
                    if attempts >= max_attempts {
                        error!("✗ Wait for response timeout");
                        break;
                    }
                }
                Err(e) => {
                    error!("✗ Receive error: {}", e);
                    break;
                }
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    // Get statistics
    let stats = {
        let engine = engine.lock().await;
        engine.stats().clone()
    };

    info!("=== Connection Statistics ===");
    info!("Bytes sent: {}", stats.bytes_sent);
    info!("Bytes received: {}", stats.bytes_received);
    info!("Packets sent: {}", stats.packets_sent);
    info!("Packets received: {}", stats.packets_received);
    info!("RTT: {}ms", stats.rtt);
    info!("RTO: {}ms", stats.rto);

    // Cleanup
    recv_task.abort();
    update_task.abort();

    info!("✓ Interoperability test completed");
    Ok(())
}

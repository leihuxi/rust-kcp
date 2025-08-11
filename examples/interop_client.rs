//! KCP Rust client interoperability test with C server
//!
//! Tests compatibility between Rust async KCP implementation and original C version

use kcp_tokio::async_kcp::KcpStream;
use kcp_tokio::config::KcpConfig;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    let server_addr: SocketAddr = "127.0.0.1:8888".parse()?;

    info!("=== KCP Rust-C Interoperability Test ===");
    info!("Connecting to C server: {}", server_addr);

    // Create KCP config - use same settings as C server
    let config = KcpConfig::new()
        .fast_mode() // nodelay=1, interval=10, resend=2, nc=1
        .window_size(128, 128)
        .mtu(1400);

    // Connect to C server
    let mut stream = match KcpStream::connect(server_addr, config).await {
        Ok(stream) => {
            info!("✓ Successfully connected to C server");
            stream
        }
        Err(e) => {
            error!("✗ Connection failed: {}", e);
            return Err(e.into());
        }
    };

    // Test message list
    let test_messages = [
        "Hello from Rust!",
        "KCP interoperability test",
        "This is message #3",
        "Testing UTF-8 message",
        "Final test message",
    ];

    info!("Starting to send test messages...");

    for (i, message) in test_messages.iter().enumerate() {
        info!("Sending message {}: {}", i + 1, message);

        // Send message
        match stream.write_all(message.as_bytes()).await {
            Ok(()) => {
                info!("✓ Message sent");
            }
            Err(e) => {
                error!("✗ Send failed: {}", e);
                continue;
            }
        }

        // Flush to ensure sending
        if let Err(e) = stream.flush().await {
            error!("✗ Flush failed: {}", e);
            continue;
        }

        // Wait for response
        let mut buffer = [0u8; 1024];
        match tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            stream.read(&mut buffer),
        )
        .await
        {
            Ok(Ok(0)) => {
                warn!("⚠ Connection closed");
                break;
            }
            Ok(Ok(n)) => {
                let response = String::from_utf8_lossy(&buffer[..n]);
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
            }
            Ok(Err(e)) => {
                error!("✗ Read error: {}", e);
            }
            Err(_) => {
                error!("✗ Read timeout");
            }
        }

        // Interval between messages
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    // Get connection statistics
    let stats = stream.stats().await;
    info!("=== Connection Statistics ===");
    info!("Bytes sent: {}", stats.bytes_sent);
    info!("Bytes received: {}", stats.bytes_received);
    info!("Packets sent: {}", stats.packets_sent);
    info!("Packets received: {}", stats.packets_received);
    info!("RTT: {}ms", stats.rtt);
    info!("RTO: {}ms", stats.rto);

    // Graceful shutdown
    if let Err(e) = stream.close().await {
        warn!("Error closing connection: {}", e);
    }

    info!("✓ Interoperability test completed");
    Ok(())
}

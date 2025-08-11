//! Simple async echo example using KCP

use kcp_tokio::async_kcp::{KcpListener, KcpStream};
use kcp_tokio::config::KcpConfig;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <server|client> [address]", args[0]);
        eprintln!("Example: {} server 127.0.0.1:12345", args[0]);
        eprintln!("Example: {} client 127.0.0.1:12345", args[0]);
        return Ok(());
    }

    let mode = &args[1];
    let addr: SocketAddr = if args.len() > 2 {
        args[2].parse()?
    } else {
        "127.0.0.1:12345".parse()?
    };

    match mode.as_str() {
        "server" => run_server(addr).await,
        "client" => run_client(addr).await,
        _ => {
            eprintln!("Mode must be 'server' or 'client'");
            Ok(())
        }
    }
}

async fn run_server(addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting KCP echo server on {}", addr);

    let config = KcpConfig::realtime();
    let mut listener = KcpListener::bind(addr, config).await?;

    info!("Server listening on {}", listener.local_addr());

    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                info!("New connection from {}", peer_addr);

                // Spawn a task to handle this connection
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream).await {
                        error!("Error handling client {}: {}", peer_addr, e);
                    }
                });
            }
            Err(e) => {
                error!("Failed to accept connection: {}", e);
            }
        }
    }
}

async fn handle_client(mut stream: KcpStream) -> Result<(), Box<dyn std::error::Error>> {
    let peer_addr = stream.peer_addr();
    info!("Handling client {}", peer_addr);

    let mut buffer = [0u8; 1024];

    loop {
        // Read data from the client
        match stream.read(&mut buffer).await {
            Ok(0) => {
                info!("Client {} disconnected", peer_addr);
                break;
            }
            Ok(n) => {
                let message = String::from_utf8_lossy(&buffer[..n]);
                info!("Received from {}: {}", peer_addr, message.trim());

                // Echo the message back
                stream.write_all(&buffer[..n]).await?;
                stream.flush().await?;

                info!("Echoed back to {}: {}", peer_addr, message.trim());
            }
            Err(e) => {
                error!("Error reading from {}: {}", peer_addr, e);
                break;
            }
        }
    }

    Ok(())
}

async fn run_client(addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    info!("Connecting to KCP server at {}", addr);

    // Use same config as interop_client for C server compatibility
    let config = KcpConfig::new().fast_mode().window_size(128, 128).mtu(1400);
    let mut stream = KcpStream::connect(addr, config).await?;

    info!("Connected to server at {}", stream.peer_addr());

    // Send some test messages
    let messages = vec![
        "Hello, KCP!",
        "This is a test message",
        "How are you doing?",
        "KCP is working great!",
        "Final test message",
    ];

    for message in messages {
        info!("Sending: {}", message);

        // Send the message
        stream.write_all(message.as_bytes()).await?;
        stream.flush().await?;

        // Read the echo response
        let mut buffer = [0u8; 1024];
        let n = stream.read(&mut buffer).await?;

        let response = String::from_utf8_lossy(&buffer[..n]);
        info!("Received echo: {}", response);

        // Verify the echo
        if response == message {
            info!("✓ Echo verified");
        } else {
            error!(
                "✗ Echo mismatch: expected '{}', got '{}'",
                message, response
            );
        }

        // Wait a bit between messages
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    info!("Client finished successfully");
    Ok(())
}

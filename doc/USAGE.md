# KCP-Rust Usage Guide

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
kcp-tokio = "0.3"
tokio = { version = "1.0", features = ["full"] }
```

## Quick Start

### Client Example

```rust
use kcp_tokio::{KcpConfig, async_kcp::KcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to server
    let addr = "127.0.0.1:8080".parse()?;
    let config = KcpConfig::new().fast_mode();
    let mut stream = KcpStream::connect(addr, config).await?;

    // Send data
    stream.write_all(b"Hello, KCP!").await?;

    // Receive response
    let mut buffer = [0u8; 1024];
    let n = stream.read(&mut buffer).await?;
    println!("Received: {}", String::from_utf8_lossy(&buffer[..n]));

    Ok(())
}
```

### Server Example

```rust
use kcp_tokio::{KcpConfig, async_kcp::KcpListener};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Bind listener
    let addr = "127.0.0.1:8080".parse()?;
    let config = KcpConfig::new().fast_mode();
    let mut listener = KcpListener::bind(addr, config).await?;

    println!("Server listening on {}", addr);

    // Accept connections
    loop {
        let (mut stream, peer_addr) = listener.accept().await?;
        println!("New connection from {}", peer_addr);

        tokio::spawn(async move {
            let mut buffer = [0u8; 1024];
            loop {
                match stream.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(n) => {
                        // Echo back
                        if stream.write_all(&buffer[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }
}
```

## Configuration

### Using Builder Pattern

```rust
use kcp_tokio::KcpConfig;
use std::time::Duration;

let config = KcpConfig::new()
    .mtu(1400)                              // MTU size
    .window_size(128, 128)                  // Send/receive windows
    .fast_mode()                            // Enable fast mode
    .connect_timeout(Duration::from_secs(5))
    .keep_alive(Some(Duration::from_secs(30)))
    .max_retries(20);
```

### Preset Configurations

```rust
// For gaming - ultra-low latency
let config = KcpConfig::gaming();

// For file transfers - high throughput
let config = KcpConfig::file_transfer();

// For real-time communication
let config = KcpConfig::realtime();

// For testing with simulated packet loss
let config = KcpConfig::testing(0.1); // 10% packet loss
```

### Performance Modes

```rust
// Normal mode (default) - 40ms update interval
let config = KcpConfig::new().normal_mode();

// Fast mode - 8ms update interval
let config = KcpConfig::new().fast_mode();

// Turbo mode - 4ms update interval, no congestion control
let config = KcpConfig::new().turbo_mode();

// Custom configuration
use kcp_tokio::config::NodeDelayConfig;

let config = KcpConfig::new()
    .nodelay_config(NodeDelayConfig::custom(
        true,  // nodelay
        5,     // interval (ms)
        1,     // resend threshold
        true,  // no congestion control
    ));
```

## API Usage

### Using AsyncRead/AsyncWrite (Tokio Traits)

```rust
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Write data
stream.write_all(b"Hello").await?;

// Read data
let mut buf = [0u8; 1024];
let n = stream.read(&mut buf).await?;

// Read exact amount
let mut buf = [0u8; 10];
stream.read_exact(&mut buf).await?;
```

### Using Native KCP Methods

```rust
use bytes::Bytes;

// Send raw bytes
stream.send(b"Hello").await?;

// Receive complete message
if let Some(data) = stream.recv().await? {
    println!("Received: {:?}", data);
}
```

### Connection Management

```rust
// Get peer address
let peer = stream.peer_addr();

// Close connection gracefully
stream.close().await?;
```

## Advanced Usage

### Custom Output Function

For special transport requirements:

```rust
use kcp_tokio::async_kcp::engine::{KcpEngine, OutputFn};
use std::sync::Arc;
use bytes::Bytes;

let output_fn: OutputFn = Arc::new(move |data: Bytes| {
    let socket = socket.clone();
    Box::pin(async move {
        // Custom packet sending logic
        socket.send_to(&data, target_addr).await?;
        Ok(())
    })
});

engine.set_output(output_fn);
```

### Monitoring Statistics

```rust
// Per-connection statistics
let stats = stream.engine.lock().await.stats();
println!("RTT: {}ms", stats.rtt);
println!("Packets sent: {}", stats.packets_sent);
println!("Retransmissions: {}", stats.retransmissions);

// Global metrics
use kcp_tokio::metrics::global_metrics;

let metrics = global_metrics().snapshot();
println!("Active connections: {}", metrics.active_connections);
println!("Total bytes sent: {}", metrics.total_bytes_sent);
```

### Buffer Pool Statistics

```rust
use kcp_tokio::common::buffer_pool_stats;

for (name, hits, size) in buffer_pool_stats() {
    println!("{}: {} hits, {} cached", name, hits, size);
}
```

## Error Handling

```rust
use kcp_tokio::{KcpError, Result};

async fn handle_connection() -> Result<()> {
    let stream = KcpStream::connect(addr, config).await?;

    match stream.send(b"test").await {
        Ok(()) => println!("Sent successfully"),
        Err(KcpError::Io(e)) => println!("I/O error: {}", e),
        Err(KcpError::Connection(e)) => println!("Connection error: {:?}", e),
        Err(KcpError::Timeout(_)) => println!("Operation timed out"),
        Err(e) => println!("Other error: {}", e),
    }

    Ok(())
}
```

## Best Practices

### 1. Choose the Right Configuration

| Use Case | Recommended Config | Why |
|----------|-------------------|-----|
| Gaming | `KcpConfig::gaming()` | Ultra-low latency (3ms) |
| VoIP | `KcpConfig::realtime()` | Low latency with reliability |
| File Transfer | `KcpConfig::file_transfer()` | High throughput |
| General | `KcpConfig::new().fast_mode()` | Balanced performance |

### 2. Handle Backpressure

```rust
// The data channel has a buffer of 256 messages
// If you're sending faster than receiving, consider:

// Option 1: Check if send would block
if stream.try_send(data).is_err() {
    // Handle backpressure
}

// Option 2: Use bounded sends with timeout
use tokio::time::timeout;
timeout(Duration::from_millis(100), stream.send(data)).await??;
```

### 3. Graceful Shutdown

```rust
// Always close connections properly
stream.close().await?;

// Or use drop for automatic cleanup
drop(stream);
```

### 4. Logging and Debugging

Enable tracing for debugging:

```rust
// In your main function
tracing_subscriber::fmt()
    .with_max_level(tracing::Level::DEBUG)
    .init();
```

Set log level via environment:

```bash
RUST_LOG=kcp_tokio=debug cargo run
```

## Common Patterns

### Echo Server

```rust
async fn echo_handler(mut stream: KcpStream) {
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                if stream.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}
```

### Request-Response

```rust
async fn request_response(
    stream: &mut KcpStream,
    request: &[u8],
) -> Result<Bytes> {
    stream.send(request).await?;

    match stream.recv().await? {
        Some(response) => Ok(response),
        None => Err(KcpError::timeout("No response")),
    }
}
```

### Heartbeat/Keep-Alive

```rust
async fn with_heartbeat(mut stream: KcpStream) {
    let mut interval = tokio::time::interval(Duration::from_secs(10));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if stream.send(b"PING").await.is_err() {
                    break;
                }
            }
            result = stream.recv() => {
                match result {
                    Ok(Some(data)) => handle_data(data),
                    Ok(None) => continue,
                    Err(_) => break,
                }
            }
        }
    }
}
```

## Troubleshooting

### Connection Refused

```rust
// Ensure server is running before client connects
// Server must be bound before client attempts connection
```

### High Latency

```rust
// Use faster mode
let config = KcpConfig::new().turbo_mode();

// Or tune manually
let config = KcpConfig::new()
    .nodelay_config(NodeDelayConfig::custom(true, 3, 1, true));
```

### Memory Usage

```rust
// Monitor buffer pool usage
let stats = buffer_pool_stats();
// If pools are exhausted, consider:
// 1. Reducing concurrent connections
// 2. Processing data faster
// 3. Using larger buffer sizes
```

### Packet Loss

```rust
// For lossy networks, use more conservative settings
let config = KcpConfig::new()
    .fast_mode()
    .window_size(64, 64)  // Smaller windows
    .max_retries(30);     // More retries
```

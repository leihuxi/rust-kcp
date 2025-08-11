# KCP Rust

[![Crates.io](https://img.shields.io/crates/v/kcp-tokio.svg)](https://crates.io/crates/kcp-tokio)
[![Documentation](https://docs.rs/kcp-tokio/badge.svg)](https://docs.rs/kcp-tokio)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)


A high-performance async Rust implementation of KCP - A Fast and Reliable ARQ Protocol built on top of Tokio.

## 🚀 Features

- **Async-First Design**: Built from ground up for async/await with Tokio integration
- **Zero-Copy**: Efficient buffer management using the `bytes` crate
- **Connection-Oriented**: High-level connection abstractions (`KcpStream`, `KcpListener`)
- **Backward Compatible**: Protocol-level compatibility with original C implementation
- **Observability**: Integrated tracing and metrics support
- **Memory Efficient**: Object pooling and buffer reuse
- **Multiple Performance Modes**: Normal, Fast, Turbo, and specialized presets
- **Flexible Configuration**: Comprehensive configuration options for different use cases

## 📦 Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
kcp-tokio = "0.3.1"
```

## 🎯 Quick Start

### Basic Echo Server

```rust
use kcp_tokio::{KcpConfig, async_kcp::{KcpListener, KcpStream}};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Server
    let config = KcpConfig::realtime();
    let mut listener = KcpListener::bind("127.0.0.1:12345", config).await?;
    
    while let Ok((mut stream, addr)) = listener.accept().await {
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            while let Ok(n) = stream.read(&mut buf).await {
                if n == 0 { break; }
                stream.write_all(&buf[..n]).await.unwrap();
            }
        });
    }
    
    Ok(())
}
```

### Basic Client

```rust
use kcp_tokio::{KcpConfig, async_kcp::KcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = KcpConfig::new().fast_mode();
    let mut stream = KcpStream::connect("127.0.0.1:12345", config).await?;
    
    // Send data
    stream.write_all(b"Hello, KCP!").await?;
    
    // Receive echo
    let mut buffer = [0u8; 1024];
    let n = stream.read(&mut buffer).await?;
    println!("Received: {}", String::from_utf8_lossy(&buffer[..n]));
    
    Ok(())
}
```

## 🏗️ Architecture

This implementation features a layered architecture designed for performance and maintainability:

```
┌─────────────────────┐
│   High-Level API    │  KcpStream, KcpListener
├─────────────────────┤
│   Connection Layer  │  KcpConnection, Session Management  
├─────────────────────┤
│   Protocol Core     │  Async KCP Engine
├─────────────────────┤
│   Transport Layer   │  UDP Socket, Packet I/O
└─────────────────────┘
```

### Key Components

- **KcpStream/KcpListener**: High-level async I/O interfaces
- **KcpConnection**: Connection state management and packet routing
- **KcpEngine**: Core KCP protocol implementation with async support
- **KcpConfig**: Comprehensive configuration system

## ⚙️ Configuration

KCP Rust provides flexible configuration options for different use cases:

### Performance Presets

```rust
// For gaming applications - ultra-low latency
let config = KcpConfig::gaming();

// For file transfers - high throughput
let config = KcpConfig::file_transfer();

// For real-time communication - balanced
let config = KcpConfig::realtime();

// For testing with packet loss simulation
let config = KcpConfig::testing(0.1); // 10% packet loss
```

### Custom Configuration

```rust
let config = KcpConfig::new()
    .fast_mode()                    // Low latency mode
    .window_size(128, 128)         // Send/Receive windows
    .mtu(1400)                     // Maximum transmission unit
    .connect_timeout(Duration::from_secs(10))
    .keep_alive(Some(Duration::from_secs(30)))
    .stream_mode(true);            // Enable streaming mode
```

### Performance Modes

- **Normal Mode**: Balanced performance and reliability (default)
- **Fast Mode**: Optimized for low latency applications
- **Turbo Mode**: Maximum performance with minimal latency

```rust
let config = KcpConfig::new()
    .turbo_mode()                  // Maximum performance
    .window_size(256, 256)         // Larger windows
    .mtu(1200);                    // Optimized MTU
```

## 📖 Examples

The `examples/` directory contains several complete examples:

### Run Echo Server/Client

```bash
# Terminal 1 - Start server
cargo run --example simple_echo server 127.0.0.1:12345

# Terminal 2 - Run client
cargo run --example simple_echo client 127.0.0.1:12345
```

### Available Examples

- **simple_echo**: Basic echo server and client
- **interop_client**: Interoperability test with C KCP implementation
- **quick_test**: Performance and reliability testing

## 🧪 Testing

Run the test suite:

```bash
# Run all tests
cargo test

# Run integration tests
cargo test --test integration_test

# Run with output
cargo test -- --nocapture
```

### Test Features

- **Integration Tests**: Full protocol testing with simulated networks
- **Performance Tests**: Throughput and latency benchmarks
- **Interoperability Tests**: Compatibility with original C implementation

## 🎯 Use Cases

### Gaming
- Ultra-low latency communication
- Reliable packet delivery for game state
- Custom congestion control

### Real-time Communication
- Voice/video streaming protocols
- Live data feeds
- Interactive applications

### File Transfer
- Reliable bulk data transfer
- Resume capability
- Network-adaptive throughput

## 🔧 Advanced Features

### Stream Mode
```rust
let config = KcpConfig::new().stream_mode(true);
```
Enables TCP-like streaming without message boundaries.

### Packet Loss Simulation
```rust
let config = KcpConfig::new().simulate_packet_loss(0.05); // 5% loss
```
Useful for testing network resilience.

### Custom Socket Buffers
```rust
let config = KcpConfig::new().socket_buffer_size(64 * 1024); // 64KB buffers
```

## 📊 Performance

KCP Rust is optimized for:
- **Low Latency**: Typically 30-40% lower than TCP
- **High Throughput**: Efficient zero-copy operations
- **Memory Efficiency**: Object pooling and buffer reuse
- **CPU Efficiency**: Async/await reduces context switching

## 🔗 Compatibility

- **Protocol**: Compatible with original C KCP implementation
- **Rust**: Requires Rust 1.70+
- **Tokio**: Built on Tokio 1.0+ for async runtime

## 📝 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## 🤝 Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/AmazingFeature`)
3. Commit your changes (`git commit -m 'Add some AmazingFeature'`)
4. Push to the branch (`git push origin feature/AmazingFeature`)
5. Open a Pull Request

## 📚 Resources

- [Original KCP Protocol](https://github.com/skywind3000/kcp)
- [KCP Protocol Documentation](https://github.com/skywind3000/kcp/blob/master/README.en.md)
- [Tokio Documentation](https://tokio.rs/)

## 🏷️ Version History

- **v0.3.1**: Current version with full async support and comprehensive configuration
- **v0.2.x**: Improved performance and bug fixes
- **v0.1.x**: Initial implementation

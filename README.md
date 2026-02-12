# KCP Rust

[![Crates.io](https://img.shields.io/crates/v/kcp-tokio.svg)](https://crates.io/crates/kcp-tokio)
[![Documentation](https://docs.rs/kcp-tokio/badge.svg)](https://docs.rs/kcp-tokio)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

A high-performance async Rust implementation of KCP - A Fast and Reliable ARQ Protocol built on top of Tokio.

## Features

- **Async-First Design**: Built from ground up for async/await with Tokio integration
- **Zero-Copy**: Efficient buffer management using the `bytes` crate
- **Lock-Free Buffer Pool**: High-performance memory management with `crossbeam`
- **Connection-Oriented**: High-level connection abstractions (`KcpStream`, `KcpListener`)
- **Protocol Compatible**: Compatible with original C KCP implementation
- **Observability**: Integrated tracing and metrics support
- **Memory Efficient**: Object pooling and buffer reuse
- **Multiple Performance Modes**: Normal, Fast, Turbo, Gaming presets

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
kcp-tokio = "0.3.7"
tokio = { version = "1.0", features = ["full"] }
```

## Quick Start

### Client

```rust
use kcp_tokio::{KcpConfig, async_kcp::KcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = KcpConfig::new().fast_mode();
    let mut stream = KcpStream::connect("127.0.0.1:12345".parse()?, config).await?;

    // Send data
    stream.write_all(b"Hello, KCP!").await?;

    // Receive response
    let mut buffer = [0u8; 1024];
    let n = stream.read(&mut buffer).await?;
    println!("Received: {}", String::from_utf8_lossy(&buffer[..n]));

    Ok(())
}
```

### Server

```rust
use kcp_tokio::{KcpConfig, async_kcp::KcpListener};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = KcpConfig::realtime();
    let mut listener = KcpListener::bind("127.0.0.1:12345".parse()?, config).await?;

    println!("Server listening on 127.0.0.1:12345");

    while let Ok((mut stream, addr)) = listener.accept().await {
        println!("New connection from {}", addr);
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            while let Ok(n) = stream.read(&mut buf).await {
                if n == 0 { break; }
                let _ = stream.write_all(&buf[..n]).await;
            }
        });
    }

    Ok(())
}
```

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Application Layer                         │
│              (User code using KcpStream/KcpListener)         │
├─────────────────────────────────────────────────────────────┤
│                    High-Level API Layer                      │
│                  KcpStream    KcpListener                    │
│           (AsyncRead/AsyncWrite, TCP-like interface)         │
├─────────────────────────────────────────────────────────────┤
│                    Protocol Core Layer                       │
│                       KcpEngine                              │
│        (ARQ logic, congestion control, retransmission)       │
├─────────────────────────────────────────────────────────────┤
│                    Common Layer                              │
│         KcpSegment, KcpHeader, BufferPool, Constants         │
├─────────────────────────────────────────────────────────────┤
│                    Transport Layer                           │
│          Generic Transport trait (UDP default)               │
└─────────────────────────────────────────────────────────────┘
```

## Configuration

### Performance Presets

```rust
// Gaming - ultra-low latency (3ms update interval)
let config = KcpConfig::gaming();

// Real-time communication (8ms update interval)
let config = KcpConfig::realtime();

// File transfer - high throughput
let config = KcpConfig::file_transfer();

// Testing with packet loss simulation
let config = KcpConfig::testing(0.1); // 10% packet loss
```

### Performance Modes

| Mode | Update Interval | Resend | Congestion Control | Use Case |
|------|----------------|--------|-------------------|----------|
| Normal | 40ms | 0 | Yes | General purpose |
| Fast | 8ms | 2 | Yes | Low latency |
| Turbo | 4ms | 1 | No | Maximum speed |
| Gaming | 3ms | 1 | No | Real-time games |

### Custom Configuration

```rust
use std::time::Duration;

let config = KcpConfig::new()
    .fast_mode()
    .window_size(128, 128)
    .mtu(1400)
    .connect_timeout(Duration::from_secs(10))
    .keep_alive(Some(Duration::from_secs(30)))
    .stream_mode(true);
```

## Examples

```bash
# Run performance test server
cargo run --example perf_test_server -- 127.0.0.1:12345 gaming

# Run performance test client
cargo run --example perf_test_client -- 127.0.0.1:12345

# Run simple echo example
cargo run --example simple_echo
```

## Testing

```bash
# Run all tests
cargo test

# Run resilience tests (packet loss, reorder, concurrent connections)
cargo test --test resilience_test

# Run benchmarks
cargo bench

# Run with logging
RUST_LOG=debug cargo test -- --nocapture

# Run clippy
cargo clippy --all-targets -- --deny clippy::all
```

## Documentation

Detailed documentation is available in the [`doc/`](doc/) directory:

| Document | Description |
|----------|-------------|
| [ARCHITECTURE.md](doc/ARCHITECTURE.md) | System architecture and design |
| [MODULES.md](doc/MODULES.md) | Module reference and APIs |
| [USAGE.md](doc/USAGE.md) | Usage guide and examples |
| [TESTING.md](doc/TESTING.md) | Testing guide |

## Performance

KCP provides significant latency improvements over TCP:

- **30-40% lower latency** in typical network conditions
- **Better performance** on lossy networks
- **Configurable trade-offs** between latency and bandwidth

### Optimizations in this Implementation

- **Actor-based lock-free architecture**: KcpEngine runs in a single dedicated tokio task, eliminating `Arc<Mutex<>>` contention
- **Generic Transport trait**: Associated `Addr` type with RPITIT — zero heap allocation on hot path (no `Pin<Box<dyn Future>>`)
- **DashMap for packet routing**: Listener uses lock-free concurrent hashmap on the hot path
- **Lock-free buffer pools**: `crossbeam::queue::ArrayQueue` for zero-allocation fast path
- **BTreeMap receive buffer**: O(log n) insertion for out-of-order packets (vs O(n) linear scan)
- **Zero-copy segment encoding**: Flush avoids cloning segments, encodes by reference
- **Cached timestamps**: Single syscall per `input()` call instead of 3+
- **Pre-allocated buffers**: `VecDeque::with_capacity` based on window sizes, avoiding grow overhead on send burst
- **Zero-copy packet handling** with `bytes` crate
- Grouped state structs for better cache locality
- Configurable update intervals (3-40ms)
- Batch ACK processing

## Use Cases

- **Gaming**: Ultra-low latency for real-time multiplayer
- **VoIP/Video**: Real-time communication
- **Live Streaming**: Low-latency data delivery
- **File Transfer**: Reliable bulk data transfer
- **IoT**: Efficient communication for constrained devices

## Compatibility

- **Protocol**: Compatible with [original C KCP](https://github.com/skywind3000/kcp)
- **Rust**: Edition 2021, stable toolchain
- **Tokio**: 1.0+

## License

MIT License - see [LICENSE](LICENSE) file.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## Resources

- [Original KCP Protocol](https://github.com/skywind3000/kcp)
- [KCP Protocol Documentation](https://github.com/skywind3000/kcp/blob/master/README.en.md)
- [Tokio Documentation](https://tokio.rs/)

## Benchmarks

Criterion benchmarks measure engine-level throughput and latency:

```bash
cargo bench
```

| Benchmark | Description |
|-----------|-------------|
| `engine_throughput` | 10/100/500 x 1KB messages |
| `engine_small_messages` | 1000 x 64B messages |
| `engine_large_message` | Single 16KB/64KB message fragmentation + reassembly |

## Version History

- **v0.3.7** (next): Fix ACK window/UNA fields (critical flow control bug), generic Transport trait with RPITIT, resilience tests, criterion benchmarks, VecDeque pre-allocation, retransmission stats tracking
- **v0.3.7**: Actor-based lock-free architecture, DashMap packet routing, BTreeMap receive buffer, zero-copy segment encoding, timestamp caching
- **v0.3.4**: Engine refactoring, lock-free buffer pools, documentation
- **v0.3.3**: Performance optimizations, sub-millisecond latency
- **v0.3.1**: Full async support, comprehensive configuration
- **v0.2.x**: Performance improvements and bug fixes
- **v0.1.x**: Initial implementation

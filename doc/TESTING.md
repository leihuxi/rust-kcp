# KCP-Rust Testing Guide

## Test Overview

The project includes comprehensive tests organized into:

- **Unit Tests** - Core library functionality
- **Integration Tests** - End-to-end scenarios
- **Debug Tests** - Protocol debugging and verification
- **Performance Tests** - Benchmarks and load testing

## Running Tests

### All Tests

```bash
# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run specific test
cargo test test_name

# Run tests matching pattern
cargo test echo
```

### Unit Tests Only

```bash
cargo test --lib
```

### Integration Tests Only

```bash
cargo test --test integration_test
```

### With Logging

```bash
RUST_LOG=debug cargo test -- --nocapture
RUST_LOG=trace cargo test test_echo_debug -- --nocapture
```

## Test Categories

### Unit Tests (`src/lib.rs`)

Basic functionality tests:

```rust
#[test]
fn test_version() {
    assert!(!VERSION.is_empty());
    assert_eq!(PROTOCOL_VERSION, 1);
}
```

Run: `cargo test --lib`

### Metrics Tests (`src/metrics.rs`)

```rust
#[test]
fn test_global_metrics() {
    let metrics = global_metrics();
    metrics.connection_created();
    let snapshot = metrics.snapshot();
    assert!(snapshot.total_connections > 0);
}

#[test]
fn test_connection_monitor() {
    // Tests connection lifecycle tracking
}
```

### Integration Tests (`tests/`)

| Test File | Description |
|-----------|-------------|
| `echo_test.rs` | Basic echo functionality |
| `integration_test.rs` | Comprehensive integration |
| `simple_echo_debug.rs` | Simple echo with debug |
| `debug_multiple_messages.rs` | Multi-message exchange |
| `test_echo_debug.rs` | Echo with native API |

### Debug Tests

Special tests for protocol debugging:

```bash
# Basic echo
cargo test debug_echo_simple -- --nocapture

# Multiple messages
cargo test debug_multiple_messages -- --nocapture

# Handshake debugging
cargo test debug_handshake_detailed -- --nocapture

# Client receive debugging
cargo test debug_client_recv -- --nocapture
```

## Test Structure

### Basic Echo Test Pattern

```rust
#[tokio::test]
async fn test_echo() {
    let server_addr = "127.0.0.1:19000";

    // Start server
    let server_handle = tokio::spawn(async move {
        let mut listener = KcpListener::bind(
            server_addr.parse().unwrap(),
            KcpConfig::realtime()
        ).await.unwrap();

        let (mut stream, _) = listener.accept().await.unwrap();

        // Echo loop
        loop {
            match stream.recv().await {
                Ok(Some(data)) => {
                    stream.send(&data).await.unwrap();
                }
                Ok(None) => continue,
                Err(_) => break,
            }
        }
    });

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Client
    let mut client = KcpStream::connect(
        server_addr.parse().unwrap(),
        KcpConfig::new().fast_mode()
    ).await.unwrap();

    // Send and verify echo
    client.send(b"Hello").await.unwrap();

    // Receive with retry
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(Some(data)) = client.recv().await {
            assert_eq!(&data[..], b"Hello");
            break;
        }
    }

    server_handle.abort();
}
```

### Multi-Message Test Pattern

```rust
#[tokio::test]
async fn debug_multiple_messages() {
    let messages = ["Msg1", "Msg2", "Msg3"];

    for msg in messages.iter() {
        // Send
        client.send(msg.as_bytes()).await.unwrap();

        // Wait for echo with retries
        for retry in 0..20 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            match client.recv().await {
                Ok(Some(data)) => {
                    assert_eq!(&data[..], msg.as_bytes());
                    break;
                }
                Ok(None) => continue,
                Err(e) => panic!("Error: {}", e),
            }
        }
    }
}
```

## Writing New Tests

### Test Template

```rust
use kcp_tokio::async_kcp::{KcpListener, KcpStream};
use kcp_tokio::config::KcpConfig;
use std::time::Duration;
use tokio::time::timeout;

#[tokio::test]
async fn test_my_feature() {
    // 1. Setup - unique port to avoid conflicts
    let server_addr = "127.0.0.1:19XXX";

    // 2. Optional: channel for coordination
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    // 3. Start server
    let server = tokio::spawn(async move {
        let mut listener = KcpListener::bind(
            server_addr.parse().unwrap(),
            KcpConfig::realtime()
        ).await.unwrap();

        ready_tx.send(()).unwrap();

        // Server logic...
    });

    // 4. Wait for server
    ready_rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 5. Client logic
    let mut client = KcpStream::connect(
        server_addr.parse().unwrap(),
        KcpConfig::new().fast_mode()
    ).await.unwrap();

    // 6. Test assertions

    // 7. Cleanup
    let _ = client.close().await;
    let _ = timeout(Duration::from_secs(1), server).await;
}
```

### Best Practices

1. **Use unique ports** - Avoid port conflicts between tests
2. **Add coordination** - Use channels to ensure server is ready
3. **Handle timing** - KCP is async, use retries for receives
4. **Cleanup properly** - Close connections, abort tasks
5. **Use timeouts** - Prevent tests from hanging

## Performance Testing

### Running Examples

```bash
# Start performance test server
cargo run --example perf_test_server -- 127.0.0.1:12345 gaming

# In another terminal, run client
cargo run --example perf_test_client -- 127.0.0.1:12345
```

### Server Modes

```bash
cargo run --example perf_test_server -- ADDRESS MODE

# Modes: normal, fast, turbo, gaming, file_transfer
```

### Performance Metrics

The server reports:
- Messages per second
- Throughput (Mbps)
- Packet loss rate
- Buffer pool statistics
- Active connections

### Simple Echo Example

```bash
cargo run --example simple_echo
```

## Debugging Tests

### Enable Tracing

```rust
#[tokio::test]
async fn test_with_tracing() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .init();

    // Test code...
}
```

### Environment Variables

```bash
# Full trace output
RUST_LOG=trace cargo test test_name -- --nocapture

# KCP library only
RUST_LOG=kcp_tokio=debug cargo test -- --nocapture

# Specific module
RUST_LOG=kcp_tokio::async_kcp::engine=trace cargo test -- --nocapture
```

### Common Debug Points

1. **Connection issues**
   ```bash
   RUST_LOG=kcp_tokio::async_kcp::stream=debug cargo test
   ```

2. **Protocol issues**
   ```bash
   RUST_LOG=kcp_tokio::async_kcp::engine=trace cargo test
   ```

3. **Listener issues**
   ```bash
   RUST_LOG=kcp_tokio::async_kcp::listener=debug cargo test
   ```

## Test Configuration

### Timeout Settings

```rust
use tokio::time::timeout;

// Wrap operations with timeout
let result = timeout(
    Duration::from_secs(5),
    stream.recv()
).await;
```

### Packet Loss Simulation

```rust
let config = KcpConfig::testing(0.1); // 10% loss
```

### Custom Test Config

```rust
let config = KcpConfig::new()
    .fast_mode()
    .window_size(32, 32)
    .connect_timeout(Duration::from_secs(3))
    .max_retries(10);
```

## Continuous Integration

### GitHub Actions Example

```yaml
name: Tests

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
      - name: Run tests
        run: cargo test --all-features
      - name: Run clippy
        run: cargo clippy --all-targets -- -D warnings
```

## Test Coverage

### Using cargo-tarpaulin

```bash
# Install
cargo install cargo-tarpaulin

# Run coverage
cargo tarpaulin --out Html

# View report
open tarpaulin-report.html
```

### Using llvm-cov

```bash
# Install
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov

# Run coverage
cargo llvm-cov --html

# View report
open target/llvm-cov/html/index.html
```

## Troubleshooting Tests

### Tests Hanging

```rust
// Add timeouts to all async operations
let result = timeout(Duration::from_secs(5), async_operation).await;
```

### Port Already in Use

```rust
// Use different ports for each test
let port = 19000 + rand::random::<u16>() % 1000;
let addr = format!("127.0.0.1:{}", port);
```

### Flaky Tests

```rust
// Add retries for timing-sensitive operations
for attempt in 0..10 {
    if let Ok(Some(data)) = stream.recv().await {
        // Success
        break;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
}
```

### Server Not Ready

```rust
// Use channels for synchronization
let (tx, rx) = tokio::sync::oneshot::channel();

let server = tokio::spawn(async move {
    let listener = KcpListener::bind(...).await.unwrap();
    tx.send(()).unwrap(); // Signal ready
    // ...
});

rx.await.unwrap(); // Wait for server
```

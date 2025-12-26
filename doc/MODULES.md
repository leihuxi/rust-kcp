# KCP-Rust Module Reference

## Module Overview

```
kcp-tokio/
├── src/
│   ├── lib.rs              # Crate root, re-exports
│   ├── config.rs           # Configuration types
│   ├── error.rs            # Error types
│   ├── common.rs           # Shared types and utilities
│   ├── metrics.rs          # Performance monitoring
│   └── async_kcp/
│       ├── mod.rs          # Module exports
│       ├── engine.rs       # KCP protocol core
│       ├── stream.rs       # High-level stream API
│       └── listener.rs     # Server listener
```

---

## `lib.rs` - Crate Root

**Purpose:** Main entry point, re-exports public API.

### Exports

```rust
// Modules
pub mod async_kcp;
pub mod common;
pub mod config;
pub mod error;
pub mod metrics;

// Re-exports
pub use config::KcpConfig;
pub use error::{KcpError, Result};
pub use async_kcp::*;

// Constants
pub const VERSION: &str;
pub const PROTOCOL_VERSION: u32;
```

---

## `config.rs` - Configuration

**Purpose:** KCP configuration with builder pattern.

### Types

#### `KcpConfig`

Main configuration struct:

```rust
pub struct KcpConfig {
    pub mtu: u32,                          // Maximum transmission unit (default: 1400)
    pub snd_wnd: u32,                      // Send window size (default: 32)
    pub rcv_wnd: u32,                      // Receive window size (default: 128)
    pub nodelay: NodeDelayConfig,          // Delay configuration
    pub connect_timeout: Duration,         // Connection timeout (default: 10s)
    pub keep_alive: Option<Duration>,      // Keep-alive interval
    pub max_retries: u32,                  // Max retransmissions (default: 20)
    pub stream_mode: bool,                 // Stream mode flag
    pub socket_buffer_size: Option<usize>, // UDP buffer size
    pub simulate_packet_loss: Option<f32>, // Testing: packet loss rate
}
```

#### `NodeDelayConfig`

Performance mode configuration:

```rust
pub struct NodeDelayConfig {
    pub nodelay: bool,              // Enable no-delay mode
    pub interval: u32,              // Update interval (ms)
    pub resend: u32,                // Fast resend threshold
    pub no_congestion_control: bool, // Disable CC
}
```

### Methods

| Method | Description |
|--------|-------------|
| `KcpConfig::new()` | Create with defaults |
| `KcpConfig::gaming()` | Gaming preset (3ms interval) |
| `KcpConfig::file_transfer()` | File transfer preset |
| `KcpConfig::realtime()` | Real-time communication preset |
| `.fast_mode()` | Enable fast mode (8ms) |
| `.turbo_mode()` | Enable turbo mode (4ms) |
| `.window_size(snd, rcv)` | Set window sizes |
| `.mtu(size)` | Set MTU |
| `.validate()` | Validate configuration |

### Preset Comparison

| Preset | Interval | Resend | CC | Use Case |
|--------|----------|--------|-----|----------|
| Normal | 40ms | 0 | Yes | General |
| Fast | 8ms | 2 | Yes | Low latency |
| Turbo | 4ms | 1 | No | Maximum speed |
| Gaming | 3ms | 1 | No | Real-time games |

---

## `error.rs` - Error Types

**Purpose:** Error handling types.

### Types

#### `KcpError`

```rust
pub enum KcpError {
    Io(std::io::Error),           // I/O errors
    Protocol(String),             // Protocol violations
    Connection(ConnectionError),  // Connection errors
    Config(String),               // Configuration errors
    Buffer(String),               // Buffer errors
    Timeout(String),              // Timeout errors
}
```

#### `ConnectionError`

```rust
pub enum ConnectionError {
    NotConnected,
    AlreadyConnected,
    Closed,
    Lost,
    Refused,
}
```

### Result Type

```rust
pub type Result<T> = std::result::Result<T, KcpError>;
```

---

## `common.rs` - Shared Types

**Purpose:** Protocol types, constants, and utilities.

### Constants Module

```rust
pub mod constants {
    // RTO values
    pub const IKCP_RTO_NDL: u32 = 30;    // No-delay min RTO
    pub const IKCP_RTO_MIN: u32 = 100;   // Normal min RTO
    pub const IKCP_RTO_DEF: u32 = 200;   // Default RTO
    pub const IKCP_RTO_MAX: u32 = 60000; // Max RTO

    // Commands
    pub const IKCP_CMD_PUSH: u8 = 81;    // Data segment
    pub const IKCP_CMD_ACK: u8 = 82;     // Acknowledgment
    pub const IKCP_CMD_WASK: u8 = 83;    // Window probe request
    pub const IKCP_CMD_WINS: u8 = 84;    // Window probe response

    // Protocol parameters
    pub const IKCP_OVERHEAD: u32 = 24;   // Header size
    pub const IKCP_MTU_DEF: u32 = 1400;  // Default MTU
    pub const IKCP_WND_RCV: u32 = 128;   // Default receive window
    pub const IKCP_DEADLINK: u32 = 20;   // Max retransmissions
}
```

### Type Aliases

```rust
pub type ConvId = u32;      // Conversation ID
pub type SeqNum = u32;      // Sequence number
pub type Timestamp = u32;   // Milliseconds timestamp
```

### `KcpHeader`

24-byte protocol header:

```rust
pub struct KcpHeader {
    pub conv: ConvId,    // 4 bytes - Conversation ID
    pub cmd: u8,         // 1 byte  - Command type
    pub frg: u8,         // 1 byte  - Fragment count
    pub wnd: u16,        // 2 bytes - Window size
    pub ts: Timestamp,   // 4 bytes - Timestamp
    pub sn: SeqNum,      // 4 bytes - Sequence number
    pub una: SeqNum,     // 4 bytes - Unacknowledged seq
    pub len: u32,        // 4 bytes - Data length
}
```

### `KcpSegment`

Complete segment with header and data:

```rust
pub struct KcpSegment {
    pub header: KcpHeader,
    pub data: Bytes,

    // Internal fields
    pub resendts: Timestamp,  // Resend timestamp
    pub rto: u32,             // Retransmission timeout
    pub fastack: u32,         // Fast ACK count
    pub xmit: u32,            // Transmission count
}
```

### `KcpStats`

Connection statistics:

```rust
pub struct KcpStats {
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub packets_sent: u64,
    pub packets_received: u64,
    pub retransmissions: u64,
    pub fast_retransmissions: u64,
    pub rtt: u32,
    pub rtt_var: u32,
    pub rto: u32,
    pub snd_wnd: u32,
    pub rcv_wnd: u32,
    pub cwnd: u32,
    pub snd_buf_size: u32,
    pub rcv_buf_size: u32,
}
```

### Buffer Pool

Lock-free buffer management:

```rust
pub struct BufferPool {
    pool: ArrayQueue<BytesMut>,
    buffer_size: usize,
    hits: AtomicUsize,
}

// Global pool functions
pub fn try_get_buffer(size_hint: usize) -> BytesMut;
pub fn try_put_buffer(buf: BytesMut);
pub fn buffer_pool_stats() -> Vec<(&'static str, usize, usize)>;
```

### Utility Functions

```rust
pub fn current_timestamp() -> Timestamp;
pub fn time_diff(later: Timestamp, earlier: Timestamp) -> i32;
pub fn seq_before(seq1: SeqNum, seq2: SeqNum) -> bool;
pub fn seq_after(seq1: SeqNum, seq2: SeqNum) -> bool;
```

---

## `async_kcp/engine.rs` - Protocol Core

**Purpose:** KCP ARQ protocol implementation.

### Internal State Structs

```rust
struct RttState {
    avg: u32,      // Smoothed RTT
    var: u32,      // RTT variance
    rto: u32,      // Retransmission timeout
    min_rto: u32,  // Minimum RTO
}

struct WindowState {
    snd: u32,      // Send window
    rcv: u32,      // Receive window
    rmt: u32,      // Remote window
    cwnd: u32,     // Congestion window
    ssthresh: u32, // Slow start threshold
    incr: u32,     // Congestion avoidance increment
}

struct ProbeState {
    flags: u32,    // Probe flags
    wait: u32,     // Probe wait time
    ts: Timestamp, // Probe timestamp
}
```

### `KcpEngine`

Core protocol engine:

```rust
pub struct KcpEngine {
    conv: ConvId,
    config: KcpConfig,
    snd_una: SeqNum,
    snd_nxt: SeqNum,
    rcv_nxt: SeqNum,
    rtt: RttState,
    wnd: WindowState,
    probe: ProbeState,
    snd_queue: VecDeque<KcpSegment>,
    rcv_queue: VecDeque<KcpSegment>,
    snd_buf: VecDeque<KcpSegment>,
    rcv_buf: VecDeque<KcpSegment>,
    ack_list: Vec<(SeqNum, Timestamp)>,
    stats: KcpStats,
    output: Option<OutputFn>,
    // ...
}
```

### Public Methods

| Method | Description |
|--------|-------------|
| `new(conv, config)` | Create new engine |
| `set_output(fn)` | Set packet output function |
| `start()` | Start the engine |
| `send(data)` | Queue data for sending |
| `recv()` | Receive assembled data |
| `input(data)` | Process incoming packet |
| `flush()` | Flush pending data |
| `update()` | Periodic state update |
| `stats()` | Get current statistics |
| `is_dead()` | Check connection health |

---

## `async_kcp/stream.rs` - Stream API

**Purpose:** High-level async stream interface.

### `KcpStream`

TCP-like stream over KCP:

```rust
pub struct KcpStream {
    engine: Arc<Mutex<KcpEngine>>,
    socket: Arc<UdpSocket>,
    peer_addr: SocketAddr,
    config: KcpConfig,
    read_buf: BytesMut,
    data_rx: mpsc::Receiver<Bytes>,
    data_tx: mpsc::Sender<Bytes>,
    // ...
}
```

### Methods

| Method | Description |
|--------|-------------|
| `connect(addr, config)` | Connect to server |
| `new_with_conv(...)` | Create with specific conv ID |
| `send(data)` | Send raw bytes |
| `recv()` | Receive raw bytes |
| `peer_addr()` | Get peer address |
| `close()` | Close connection |

### Trait Implementations

- `AsyncRead` - Tokio async read
- `AsyncWrite` - Tokio async write

---

## `async_kcp/listener.rs` - Server Listener

**Purpose:** Accept incoming KCP connections.

### `KcpListener`

Server-side listener:

```rust
pub struct KcpListener {
    socket: Arc<UdpSocket>,
    config: KcpConfig,
    pending_connections: mpsc::Receiver<(KcpStream, SocketAddr)>,
    // ...
}
```

### Methods

| Method | Description |
|--------|-------------|
| `bind(addr, config)` | Bind to address |
| `accept()` | Accept new connection |
| `local_addr()` | Get bound address |

---

## `metrics.rs` - Performance Monitoring

**Purpose:** Global metrics collection.

### `GlobalMetrics`

```rust
pub struct GlobalMetrics {
    pub active_connections: u64,
    pub total_connections: u64,
    pub total_packets_sent: u64,
    pub total_packets_received: u64,
    pub total_bytes_sent: u64,
    pub total_bytes_received: u64,
    pub total_retransmissions: u64,
}
```

### Functions

```rust
pub fn global_metrics() -> &'static GlobalMetricsCollector;
```

### Methods on `GlobalMetricsCollector`

| Method | Description |
|--------|-------------|
| `connection_created()` | Record new connection |
| `connection_closed()` | Record closed connection |
| `packet_sent(bytes)` | Record sent packet |
| `packet_received(bytes)` | Record received packet |
| `retransmission()` | Record retransmission |
| `snapshot()` | Get current metrics |

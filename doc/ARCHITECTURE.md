# KCP-Rust Architecture

## Overview

KCP-Rust is a high-performance async implementation of the KCP (Fast and Reliable ARQ Protocol) built on top of Tokio. This document describes the overall architecture and design decisions.

## Layered Architecture

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
│                   UDP Socket (Tokio)                         │
└─────────────────────────────────────────────────────────────┘
```

## Core Components

### 1. KcpEngine (`src/async_kcp/engine.rs`)

The protocol core implementing KCP ARQ logic:

```rust
pub struct KcpEngine {
    // Core identifiers
    conv: ConvId,
    config: KcpConfig,

    // Sequence tracking
    snd_una: SeqNum,      // Oldest unacknowledged
    snd_nxt: SeqNum,      // Next to send
    rcv_nxt: SeqNum,      // Next expected to receive

    // Grouped state structs
    rtt: RttState,        // RTT calculation
    wnd: WindowState,     // Window management
    probe: ProbeState,    // Window probing

    // Buffers
    snd_queue: VecDeque<KcpSegment>,  // User data waiting to send
    rcv_queue: VecDeque<KcpSegment>,  // Received data for user
    snd_buf: VecDeque<KcpSegment>,    // Sent but unacknowledged
    rcv_buf: VecDeque<KcpSegment>,    // Received out-of-order
    ack_list: Vec<(SeqNum, Timestamp)>,
}
```

**Key Features:**
- Nested state structs for better code organization
- Lock-free buffer pool integration
- Configurable congestion control
- Fast retransmission support

### 2. KcpStream (`src/async_kcp/stream.rs`)

High-level async stream providing TCP-like interface:

```rust
pub struct KcpStream {
    engine: Arc<Mutex<KcpEngine>>,
    socket: Arc<UdpSocket>,
    peer_addr: SocketAddr,

    // Async data flow
    data_rx: mpsc::Receiver<Bytes>,
    data_tx: mpsc::Sender<Bytes>,

    // Background tasks
    update_task: Option<JoinHandle<()>>,
    recv_task: Option<JoinHandle<()>>,
}
```

**Implements:**
- `AsyncRead` / `AsyncWrite` traits
- Native `send()` / `recv()` methods
- Background update and receive tasks

### 3. KcpListener (`src/async_kcp/listener.rs`)

Server-side connection acceptor:

- Binds to UDP socket
- Manages multiple connections via shared socket
- Routes packets to appropriate streams by conversation ID

### 4. KcpConfig (`src/config.rs`)

Builder pattern configuration:

```rust
let config = KcpConfig::new()
    .fast_mode()
    .window_size(128, 128)
    .mtu(1400);
```

**Preset Modes:**
- `normal()` - Balanced performance
- `fast_mode()` - Low latency (8ms interval)
- `turbo_mode()` - Maximum performance (4ms interval)
- `gaming()` - Ultra-low latency (3ms interval)
- `file_transfer()` - High throughput

## Data Flow

### Send Path

```
User Data
    │
    ▼
┌─────────────┐
│ KcpStream   │ write_all() / send()
│  send()     │
└─────────────┘
    │
    ▼
┌─────────────┐
│ KcpEngine   │ Fragment → Queue → Send Buffer
│  send()     │
└─────────────┘
    │
    ▼
┌─────────────┐
│ flush()     │ Encode segments → output_fn
└─────────────┘
    │
    ▼
UDP Socket ──────► Network
```

### Receive Path

```
Network ──────► UDP Socket
                    │
                    ▼
            ┌─────────────┐
            │ recv_task   │ Background loop
            └─────────────┘
                    │
                    ▼
            ┌─────────────┐
            │ KcpEngine   │ Decode → Reorder → ACK
            │  input()    │
            └─────────────┘
                    │
                    ▼
            ┌─────────────┐
            │ data_tx     │ mpsc channel
            └─────────────┘
                    │
                    ▼
            ┌─────────────┐
            │ KcpStream   │ read() / recv()
            │  poll_read  │
            └─────────────┘
                    │
                    ▼
              User Buffer
```

## Memory Management

### Buffer Pool Architecture

Lock-free buffer pools using `crossbeam::queue::ArrayQueue`:

```
┌────────────────────────────────────────────────┐
│              Global Buffer Pools               │
├────────────┬────────────┬──────────┬──────────┤
│   SMALL    │   MEDIUM   │   LARGE  │  JUMBO   │
│  ≤1024B    │  ≤1400B    │  ≤8192B  │ ≤65536B  │
│  4000 buf  │  2000 buf  │ 1000 buf │  200 buf │
└────────────┴────────────┴──────────┴──────────┘
```

**Benefits:**
- Zero-allocation fast path for hot code
- Lock-free access via atomic operations
- Automatic size-based pool selection

## Concurrency Model

```
┌─────────────────────────────────────────────────┐
│                  User Task                       │
│         (async read/write operations)            │
└─────────────────────────────────────────────────┘
                      │
          ┌───────────┴───────────┐
          ▼                       ▼
┌─────────────────┐     ┌─────────────────┐
│   update_task   │     │    recv_task    │
│ (periodic flush)│     │ (packet receive)│
└─────────────────┘     └─────────────────┘
          │                       │
          └───────────┬───────────┘
                      ▼
            ┌─────────────────┐
            │  Arc<Mutex<     │
            │   KcpEngine>>   │
            └─────────────────┘
```

## Protocol Constants

| Constant | Value | Description |
|----------|-------|-------------|
| IKCP_OVERHEAD | 24 bytes | Header size |
| IKCP_MTU_DEF | 1400 bytes | Default MTU |
| IKCP_WND_RCV | 128 | Default receive window |
| IKCP_RTO_DEF | 200ms | Default RTO |
| IKCP_DEADLINK | 20 | Max retransmissions |

## Error Handling

Error types in `src/error.rs`:

```rust
pub enum KcpError {
    Io(std::io::Error),
    Protocol(String),
    Connection(ConnectionError),
    Config(String),
    Buffer(String),
    Timeout(String),
}
```

## Performance Optimizations

1. **Lock-free Buffer Pool** - Crossbeam ArrayQueue
2. **Batch ACK Processing** - Pre-allocated vectors
3. **Inline Helper Methods** - `mss()`, `reset_cwnd()`
4. **Grouped State Structs** - Better cache locality
5. **Configurable Update Intervals** - 3-40ms based on mode

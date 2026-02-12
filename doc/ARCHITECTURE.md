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
│       Generic Transport trait (UdpTransport default)         │
└─────────────────────────────────────────────────────────────┘
```

## Core Components

### 1. KcpEngine (`kcp-core/src/engine.rs`)

The protocol core implementing KCP ARQ logic:

All methods are synchronous (`fn`, not `async fn`). Output packets are buffered in
`output_queue` and drained by the actor via `drain_output()`.

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
    snd_queue: VecDeque<KcpSegment>,       // User data waiting to send
    rcv_queue: VecDeque<KcpSegment>,       // Received data for user
    snd_buf: VecDeque<KcpSegment>,         // Sent but unacknowledged
    rcv_buf: BTreeMap<SeqNum, KcpSegment>, // Received out-of-order (O(log n) insert)
    ack_list: Vec<(SeqNum, Timestamp)>,

    // Output queue (actor drains this after each engine call)
    output_queue: Vec<Bytes>,
}
```

**Key Features:**
- All methods synchronous — no internal locking or async
- `BTreeMap` receive buffer for O(log n) insertion of out-of-order packets
- Zero-copy segment encoding in flush (encodes by reference, no clone)
- Cached timestamps to minimize syscalls per `input()` call
- Lock-free buffer pool integration
- Configurable congestion control
- Fast retransmission support

### 2. KcpStream (`kcp/stream.rs`)

High-level async stream providing TCP-like interface. Each stream spawns a dedicated
actor task that owns the `KcpEngine` exclusively — no `Arc<Mutex<>>` needed.

Communication between the stream handle and actor uses mpsc channels (`EngineCmd`).

**Implements:**
- `AsyncRead` / `AsyncWrite` traits
- Native `send()` / `recv()` methods
- Actor-based background task (update + I/O in one loop)

### 3. KcpListener (`kcp/listener.rs`)

Server-side connection acceptor:

- Binds to UDP socket
- Manages multiple connections via shared socket
- Routes packets to appropriate streams using `DashMap` (lock-free concurrent hashmap)
- Packet routing hot path requires no async lock acquisition

### 4. KcpConfig (`kcp/config.rs`)

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

Actor-based lock-free architecture (v0.3.5+):

```
┌─────────────────────────────────────────────────┐
│                  User Task                       │
│         (async read/write operations)            │
│         KcpStream handle (no engine)             │
└─────────────────────────────────────────────────┘
          │ EngineCmd (mpsc)        ▲ data_rx (mpsc)
          ▼                         │
┌─────────────────────────────────────────────────┐
│              Actor Task (dedicated)              │
│   ┌─────────────┐  ┌──────────┐  ┌──────────┐  │
│   │  KcpEngine  │  │ input_rx │  │  timer   │  │
│   │ (owned, no  │  │ (packets │  │ (update  │  │
│   │  lock)      │  │  from    │  │  flush)  │  │
│   │             │  │  socket) │  │          │  │
│   └─────────────┘  └──────────┘  └──────────┘  │
│         │                                        │
│         ▼ drain_output()                         │
│   ┌─────────────┐                                │
│   │ Transport   │ send_to() over UDP             │
│   └─────────────┘                                │
└─────────────────────────────────────────────────┘
```

The KcpEngine lives in a single dedicated tokio task (actor). All engine methods
are synchronous. The actor handles input packets, user commands, and periodic
updates in a single `tokio::select!` loop — no mutex contention.

## Protocol Constants

| Constant | Value | Description |
|----------|-------|-------------|
| IKCP_OVERHEAD | 24 bytes | Header size |
| IKCP_MTU_DEF | 1400 bytes | Default MTU |
| IKCP_WND_RCV | 128 | Default receive window |
| IKCP_RTO_DEF | 200ms | Default RTO |
| IKCP_DEADLINK | 20 | Max retransmissions |

## Error Handling

Error types in `kcp/error.rs`:

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

## Transport Layer

The `Transport` trait (`kcp/transport.rs`) abstracts the datagram transport, allowing
KCP to run over any async datagram transport, not just UDP.

```rust
pub trait Transport: Send + Sync + 'static {
    type Addr: Addr;  // Associated address type (SocketAddr for UDP)

    fn send_to<'a>(
        &'a self, buf: &'a [u8], target: &'a Self::Addr,
    ) -> impl Future<Output = io::Result<usize>> + Send + 'a;

    fn recv_from<'a>(
        &'a self, buf: &'a mut [u8],
    ) -> impl Future<Output = io::Result<(usize, Self::Addr)>> + Send + 'a;

    fn local_addr(&self) -> io::Result<Self::Addr>;
}
```

**Key design decisions:**
- **Associated `Addr` type** with `Addr` trait bound (Clone + Eq + Hash + Send + Sync + Debug + Display)
- **RPITIT** (Return Position Impl Trait in Traits, stable since Rust 1.75) instead of
  `Pin<Box<dyn Future>>` — eliminates heap allocation on every send/receive call
- **Static dispatch** via `Arc<T>` instead of `Arc<dyn Transport>` — enables inlining

The built-in `UdpTransport` uses `tokio::net::UdpSocket` with `Addr = SocketAddr`.

## Performance Optimizations

1. **Actor-based lock-free architecture** - KcpEngine owned by single task, no `Arc<Mutex<>>`
2. **Generic Transport with RPITIT** - Zero heap allocation on send/receive hot path
3. **DashMap packet routing** - Listener uses lock-free concurrent hashmap on every incoming packet
4. **BTreeMap receive buffer** - O(log n) insert for out-of-order packets (was O(n) VecDeque scan + shift)
5. **Zero-copy segment flush** - `flush_data_segments()` encodes by reference, no `segment.clone()`
6. **Cached timestamps** - Single `current_timestamp()` syscall per `input()` (was 3+)
7. **Pre-allocated buffers** - `VecDeque::with_capacity` based on window sizes, avoiding grow on send burst
8. **Lock-free Buffer Pool** - Crossbeam ArrayQueue for zero-allocation fast path
9. **Batch ACK Processing** - Pre-allocated vectors
10. **Inline Helper Methods** - `mss()`, `reset_cwnd()`
11. **Grouped State Structs** - Better cache locality
12. **Configurable Update Intervals** - 3-40ms based on mode

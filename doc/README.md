# KCP-Rust Documentation

## Contents

| Document | Description |
|----------|-------------|
| [ARCHITECTURE.md](ARCHITECTURE.md) | System architecture, design decisions, data flow |
| [MODULES.md](MODULES.md) | Module reference, types, and APIs |
| [USAGE.md](USAGE.md) | Usage guide, examples, best practices |
| [TESTING.md](TESTING.md) | Testing guide, running tests, debugging |

## Quick Links

### Getting Started
- [Installation](USAGE.md#installation)
- [Quick Start](USAGE.md#quick-start)
- [Configuration](USAGE.md#configuration)

### Architecture
- [Layered Architecture](ARCHITECTURE.md#layered-architecture)
- [Core Components](ARCHITECTURE.md#core-components)
- [Data Flow](ARCHITECTURE.md#data-flow)

### API Reference
- [KcpConfig](MODULES.md#kcpconfig)
- [KcpStream](MODULES.md#kcpstream)
- [KcpListener](MODULES.md#kcplistener)
- [KcpEngine](MODULES.md#kcpengine)

### Testing
- [Running Tests](TESTING.md#running-tests)
- [Writing Tests](TESTING.md#writing-new-tests)
- [Debugging](TESTING.md#debugging-tests)

## Project Structure

```
kcp-rust/
├── kcp-core/                # Standalone sync protocol engine
│   └── src/
│       ├── lib.rs
│       ├── protocol.rs      # Wire types & constants
│       ├── config.rs        # KcpCoreConfig, NodeDelayConfig
│       ├── error.rs         # KcpCoreError (3 variants)
│       └── engine.rs        # KcpEngine (pure sync)
├── kcp/                     # Async runtime layer (kcp-tokio)
│   ├── lib.rs               # Crate root
│   ├── engine.rs            # Re-export from kcp-core
│   ├── actor.rs             # Actor task (owns KcpEngine)
│   ├── stream.rs            # KcpStream (AsyncRead/AsyncWrite)
│   ├── listener.rs          # KcpListener (connection acceptor)
│   ├── config.rs            # KcpConfig (extends KcpCoreConfig)
│   ├── error.rs             # KcpError (extends KcpCoreError)
│   ├── transport.rs         # Generic Transport trait + UdpTransport
│   ├── buffer_pool.rs       # Lock-free buffer pool
│   ├── common.rs            # Internal re-exports
│   └── metrics.rs           # Performance monitoring
├── tests/
│   ├── common/mod.rs        # Shared test helpers
│   ├── echo_test.rs         # Basic echo tests
│   ├── simple_test.rs       # Configuration and message tests
│   └── resilience_test.rs   # Protocol resilience (loss, reorder, concurrent)
├── benches/
│   └── kcp_bench.rs         # Criterion benchmarks
├── examples/
└── doc/
```

## Version

- Library Version: 0.4.0
- Protocol Version: 1
- Rust Edition: 2021

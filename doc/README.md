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
├── src/
│   ├── lib.rs           # Crate root
│   ├── config.rs        # Configuration
│   ├── error.rs         # Error types
│   ├── common.rs        # Shared types
│   ├── transport.rs     # Generic Transport trait + UdpTransport
│   ├── metrics.rs       # Monitoring
│   └── async_kcp/
│       ├── actor.rs     # Actor task (owns KcpEngine)
│       ├── engine.rs    # Protocol core
│       ├── stream.rs    # Stream API
│       └── listener.rs  # Server listener
├── examples/
│   ├── simple_echo.rs
│   ├── perf_test_server.rs
│   └── perf_test_client.rs
├── tests/
│   ├── echo_test.rs         # Basic echo tests
│   ├── integration_test.rs  # Comprehensive integration
│   ├── resilience_test.rs   # Protocol resilience (loss, reorder, concurrent)
│   └── ...
├── benches/
│   └── kcp_bench.rs     # Criterion benchmarks
└── doc/
    └── *.md             # Documentation
```

## Version

- Library Version: 0.3.7
- Protocol Version: 1
- Rust Edition: 2021

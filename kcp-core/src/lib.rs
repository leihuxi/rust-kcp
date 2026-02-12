//! Pure synchronous KCP protocol engine.
//!
//! This crate implements the core KCP ARQ protocol with zero runtime
//! dependencies — no tokio, no async, no I/O. It only depends on `bytes`
//! and optionally `tracing`.
//!
//! ```text
//! ┌──────────────────────────┐
//! │  kcp-core                │
//! │                          │
//! │  protocol  ← wire types │
//! │  config    ← tuning     │
//! │  error     ← 3 variants │
//! │  engine    ← state machine │
//! └──────────────────────────┘
//! ```

pub mod config;
pub mod engine;
pub mod error;
pub mod protocol;

pub use config::{KcpCoreConfig, NodeDelayConfig};
pub use engine::KcpEngine;
pub use error::{KcpCoreError, KcpCoreResult};
pub use protocol::*;

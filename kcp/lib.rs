//! # KCP Rust — High-Performance Async KCP
//!
//! A modern, async-first implementation of the KCP (Fast and Reliable ARQ Protocol)
//! built on top of Tokio.
//!
//! ## Architecture
//!
//! ```text
//! ┌───────────────────────────────────────┐
//! │  kcp-tokio  (this crate)              │
//! │                                       │
//! │  KcpStream / KcpListener  ← user API  │
//! │  actor                    ← scheduler │
//! │  transport                ← UDP I/O   │
//! ├───────────────────────────────────────┤
//! │  kcp-core  (dependency)               │
//! │                                       │
//! │  KcpEngine   ← pure sync state machine│
//! │  protocol    ← wire types & constants │
//! └───────────────────────────────────────┘
//! ```
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use kcp_tokio::{KcpConfig, KcpStream};
//! use std::net::SocketAddr;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let addr: SocketAddr = "127.0.0.1:8080".parse()?;
//!     let config = KcpConfig::new().fast_mode();
//!     let mut stream = KcpStream::connect(addr, config).await?;
//!
//!     use tokio::io::AsyncWriteExt;
//!     stream.write_all(b"Hello, KCP!").await?;
//!
//!     use tokio::io::AsyncReadExt;
//!     let mut buffer = [0u8; 1024];
//!     let n = stream.read(&mut buffer).await?;
//!     println!("Received: {:?}", &buffer[..n]);
//!
//!     Ok(())
//! }
//! ```

// ── Layer 1: Core protocol (re-exported from kcp-core) ─────────────────

/// Core protocol types, constants, and wire format.
pub use kcp_core::protocol;

/// Direct access to the standalone `kcp-core` crate.
pub use kcp_core;

// ── Layer 2: Transport & runtime infrastructure ─────────────────────────

pub mod buffer_pool;
pub mod transport;
pub use transport::{Addr, Transport};
#[cfg(feature = "tokio")]
pub use transport::UdpTransport;

// ── Layer 3: Configuration & errors (extends core with I/O concerns) ────

pub mod config;
pub mod error;
pub use config::KcpConfig;
pub use error::{KcpError, Result};

// ── Layer 4: Async KCP (actor + stream + listener) ──────────────────────

#[cfg(feature = "tokio")]
pub mod engine;
#[cfg(feature = "tokio")]
pub(crate) mod actor;
#[cfg(feature = "tokio")]
pub mod stream;
#[cfg(feature = "tokio")]
pub mod listener;

#[cfg(feature = "tokio")]
pub use stream::KcpStream;
#[cfg(feature = "tokio")]
pub use listener::KcpListener;

#[cfg(feature = "tokio")]
pub mod metrics;

// ── Internal convenience re-exports ─────────────────────────────────────

pub(crate) mod common;

// ── Version info ────────────────────────────────────────────────────────

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PROTOCOL_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert!(!VERSION.is_empty());
        assert_eq!(PROTOCOL_VERSION, 1);
    }
}

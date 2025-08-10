//! # KCP Rust - High-Performance Async Implementation
//!
//! A modern, async-first implementation of the KCP (Fast and Reliable ARQ Protocol)
//! built on top of Tokio for maximum performance and scalability.
//!
//! ## Features
//!
//! - **Async-First Design**: Built from ground up for async/await
//! - **Zero-Copy**: Efficient buffer management with `bytes` crate
//! - **Connection-Oriented**: High-level connection abstractions
//! - **Backward Compatible**: Protocol-level compatibility with original C implementation
//! - **Observability**: Integrated tracing and metrics
//! - **Memory Efficient**: Object pooling and buffer reuse
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use kcp_rust::{KcpConfig, async_kcp::KcpStream};
//! use std::net::SocketAddr;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create a KCP connection
//!     let addr: SocketAddr = "127.0.0.1:8080".parse()?;
//!     let config = KcpConfig::new().fast_mode();
//!     let mut stream = KcpStream::connect(addr, config).await?;
//!     
//!     // Send data
//!     use tokio::io::AsyncWriteExt;
//!     stream.write_all(b"Hello, KCP!").await?;
//!     
//!     // Receive data
//!     use tokio::io::AsyncReadExt;
//!     let mut buffer = [0u8; 1024];
//!     let n = stream.read(&mut buffer).await?;
//!     println!("Received: {:?}", &buffer[..n]);
//!     
//!     Ok(())
//! }
//! ```
//!
//! ## Architecture
//!
//! This implementation features a layered architecture:
//!
//! ```text
//! ┌─────────────────────┐
//! │   High-Level API    │  KcpStream, KcpListener
//! ├─────────────────────┤
//! │   Connection Layer  │  KcpConnection, Session Management  
//! ├─────────────────────┤
//! │   Protocol Core     │  Async KCP Engine
//! ├─────────────────────┤
//! │   Transport Layer   │  UDP Socket, Packet I/O
//! └─────────────────────┘
//! ```

// Main async implementation
pub mod async_kcp;
pub use async_kcp::*;

// Common types and utilities
pub mod common;
pub mod config;
pub mod error;

// Re-exports
pub use config::KcpConfig;
pub use error::{KcpError, Result};

// Version information
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PROTOCOL_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert!(VERSION.len() > 0);
        assert_eq!(PROTOCOL_VERSION, 1);
    }
}

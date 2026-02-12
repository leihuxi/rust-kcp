//! Error types for the KCP core protocol engine

use std::fmt;

/// Result type for KCP core operations
pub type KcpCoreResult<T> = std::result::Result<T, KcpCoreError>;

/// Error types produced by the KCP protocol engine.
///
/// This is intentionally minimal — only the 3 variants the engine actually produces.
#[derive(Debug)]
pub enum KcpCoreError {
    /// Protocol-level errors (invalid packets, bad state)
    Protocol { message: String },
    /// Buffer management errors (message too large, etc.)
    Buffer { message: String },
    /// Connection lost (exceeded max retransmissions)
    ConnectionLost,
}

impl KcpCoreError {
    /// Create a protocol error
    pub fn protocol(message: impl Into<String>) -> Self {
        KcpCoreError::Protocol {
            message: message.into(),
        }
    }

    /// Create a buffer error
    pub fn buffer(message: impl Into<String>) -> Self {
        KcpCoreError::Buffer {
            message: message.into(),
        }
    }

    /// Create a connection-lost error
    pub fn connection_lost() -> Self {
        KcpCoreError::ConnectionLost
    }

    /// Check if this is a fatal error that should stop the engine
    pub fn is_fatal(&self) -> bool {
        matches!(self, KcpCoreError::ConnectionLost)
    }
}

impl fmt::Display for KcpCoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KcpCoreError::Protocol { message } => write!(f, "Protocol error: {message}"),
            KcpCoreError::Buffer { message } => write!(f, "Buffer error: {message}"),
            KcpCoreError::ConnectionLost => write!(f, "Connection lost"),
        }
    }
}

impl std::error::Error for KcpCoreError {}

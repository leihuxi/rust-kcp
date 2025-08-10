//! Error types for KCP Rust implementation

use std::fmt;
use thiserror::Error;

/// Result type for KCP operations
pub type Result<T> = std::result::Result<T, KcpError>;

/// Comprehensive error types for KCP operations
#[derive(Error, Debug)]
pub enum KcpError {
    /// I/O related errors
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Protocol-level errors
    #[error("Protocol error: {message}")]
    Protocol { message: String },

    /// Connection-related errors
    #[error("Connection error: {kind}")]
    Connection { kind: ConnectionError },

    /// Timeout errors
    #[error("Operation timed out after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },

    /// Buffer management errors
    #[error("Buffer error: {message}")]
    Buffer { message: String },

    /// Configuration errors
    #[error("Configuration error: {message}")]
    Config { message: String },

    /// Session management errors
    #[error("Session error: {message}")]
    Session { message: String },

    /// Internal errors that shouldn't normally occur
    #[error("Internal error: {message}")]
    Internal { message: String },
}

/// Specific connection error types
#[derive(Debug, Clone)]
pub enum ConnectionError {
    /// Connection already closed
    Closed,
    /// Connection reset by peer
    Reset,
    /// Connection refused
    Refused,
    /// Connection lost
    Lost,
    /// Invalid conversation ID
    InvalidConv,
    /// Handshake failed
    HandshakeFailed,
    /// Connection timed out
    Timeout,
    /// Not connected
    NotConnected,
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectionError::Closed => write!(f, "connection closed"),
            ConnectionError::Reset => write!(f, "connection reset by peer"),
            ConnectionError::Refused => write!(f, "connection refused"),
            ConnectionError::Lost => write!(f, "connection lost"),
            ConnectionError::InvalidConv => write!(f, "invalid conversation ID"),
            ConnectionError::HandshakeFailed => write!(f, "handshake failed"),
            ConnectionError::Timeout => write!(f, "connection timed out"),
            ConnectionError::NotConnected => write!(f, "not connected"),
        }
    }
}

impl KcpError {
    /// Create a protocol error
    pub fn protocol(message: impl Into<String>) -> Self {
        KcpError::Protocol {
            message: message.into(),
        }
    }

    /// Create a connection error
    pub fn connection(kind: ConnectionError) -> Self {
        KcpError::Connection { kind }
    }

    /// Create a timeout error
    pub fn timeout(timeout_ms: u64) -> Self {
        KcpError::Timeout { timeout_ms }
    }

    /// Create a buffer error
    pub fn buffer(message: impl Into<String>) -> Self {
        KcpError::Buffer {
            message: message.into(),
        }
    }

    /// Create a config error
    pub fn config(message: impl Into<String>) -> Self {
        KcpError::Config {
            message: message.into(),
        }
    }

    /// Create a session error
    pub fn session(message: impl Into<String>) -> Self {
        KcpError::Session {
            message: message.into(),
        }
    }

    /// Create an internal error
    pub fn internal(message: impl Into<String>) -> Self {
        KcpError::Internal {
            message: message.into(),
        }
    }

    /// Check if this is a recoverable error
    pub fn is_recoverable(&self) -> bool {
        match self {
            KcpError::Io(e) => {
                matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                )
            }
            KcpError::Timeout { .. } => true,
            KcpError::Buffer { .. } => true,
            KcpError::Connection { kind } => {
                matches!(kind, ConnectionError::Lost)
            }
            _ => false,
        }
    }

    /// Check if this is a connection-related error
    pub fn is_connection_error(&self) -> bool {
        matches!(self, KcpError::Connection { .. })
    }

    /// Check if this error indicates the connection is closed
    pub fn is_closed(&self) -> bool {
        match self {
            KcpError::Connection { kind } => {
                matches!(
                    kind,
                    ConnectionError::Closed | ConnectionError::Reset | ConnectionError::Refused
                )
            }
            KcpError::Io(e) => {
                matches!(
                    e.kind(),
                    std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::UnexpectedEof
                )
            }
            _ => false,
        }
    }
}

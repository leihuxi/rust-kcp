//! Error types for KCP.
//!
//! [`KcpError`] extends [`kcp_core::KcpCoreError`] with I/O, timeout,
//! config, and session variants needed by the async runtime layer.

use std::fmt;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, KcpError>;

// ── Error types ─────────────────────────────────────────────────────────

#[derive(Error, Debug)]
pub enum KcpError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Protocol error: {message}")]
    Protocol { message: String },

    #[error("Connection error: {kind}")]
    Connection { kind: ConnectionError },

    #[error("Operation timed out after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },

    #[error("Buffer error: {message}")]
    Buffer { message: String },

    #[error("Configuration error: {message}")]
    Config { message: String },

    #[error("Session error: {message}")]
    Session { message: String },

    #[error("Internal error: {message}")]
    Internal { message: String },
}

#[derive(Debug, Clone)]
pub enum ConnectionError {
    Closed,
    Reset,
    Refused,
    Lost,
    InvalidConv,
    HandshakeFailed,
    Timeout,
    NotConnected,
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "connection closed"),
            Self::Reset => write!(f, "connection reset by peer"),
            Self::Refused => write!(f, "connection refused"),
            Self::Lost => write!(f, "connection lost"),
            Self::InvalidConv => write!(f, "invalid conversation ID"),
            Self::HandshakeFailed => write!(f, "handshake failed"),
            Self::Timeout => write!(f, "connection timed out"),
            Self::NotConnected => write!(f, "not connected"),
        }
    }
}

// ── Bridge: kcp-core errors → KcpError ──────────────────────────────────

impl From<kcp_core::KcpCoreError> for KcpError {
    fn from(e: kcp_core::KcpCoreError) -> Self {
        match e {
            kcp_core::KcpCoreError::Protocol { message } => Self::Protocol { message },
            kcp_core::KcpCoreError::Buffer { message } => Self::Buffer { message },
            kcp_core::KcpCoreError::ConnectionLost => {
                Self::Connection { kind: ConnectionError::Lost }
            }
        }
    }
}

// ── Constructors ────────────────────────────────────────────────────────

impl KcpError {
    pub fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol { message: message.into() }
    }

    pub fn connection(kind: ConnectionError) -> Self {
        Self::Connection { kind }
    }

    pub fn timeout(timeout_ms: u64) -> Self {
        Self::Timeout { timeout_ms }
    }

    pub fn buffer(message: impl Into<String>) -> Self {
        Self::Buffer { message: message.into() }
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::Config { message: message.into() }
    }

    pub fn session(message: impl Into<String>) -> Self {
        Self::Session { message: message.into() }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal { message: message.into() }
    }
}

// ── Predicates ──────────────────────────────────────────────────────────

impl KcpError {
    pub fn is_recoverable(&self) -> bool {
        match self {
            Self::Io(e) => matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::Interrupted
            ),
            Self::Timeout { .. } | Self::Buffer { .. } => true,
            Self::Connection { kind } => matches!(kind, ConnectionError::Lost),
            _ => false,
        }
    }

    pub fn is_connection_error(&self) -> bool {
        matches!(self, Self::Connection { .. })
    }

    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Connection {
                kind: ConnectionError::Lost | ConnectionError::Closed | ConnectionError::Reset
            } | Self::Internal { .. }
        )
    }

    pub fn is_closed(&self) -> bool {
        match self {
            Self::Connection { kind } => matches!(
                kind,
                ConnectionError::Closed | ConnectionError::Reset | ConnectionError::Refused
            ),
            Self::Io(e) => matches!(
                e.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::UnexpectedEof
            ),
            _ => false,
        }
    }
}

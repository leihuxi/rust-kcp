//! Internal convenience re-exports.
//!
//! Merges protocol types (from `kcp-core`) with buffer pool utilities
//! (local to `kcp-tokio`) so internal modules can `use crate::common::*`.

pub use crate::buffer_pool::*;
pub use kcp_core::protocol::*;

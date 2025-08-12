//! Connection manager for routing packets to server streams

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use bytes::Bytes;
use crate::error::Result;

/// Handle for a server stream connection
pub struct StreamHandle {
    pub peer_addr: SocketAddr,
    pub packet_sender: mpsc::UnboundedSender<Bytes>,
}

/// Connection manager that routes packets to appropriate streams
pub struct ConnectionManager {
    /// Map from peer address to packet sender channel
    streams: Arc<RwLock<HashMap<SocketAddr, mpsc::UnboundedSender<Bytes>>>>,
}

impl ConnectionManager {
    /// Create a new connection manager
    pub fn new() -> Self {
        Self {
            streams: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new stream
    pub async fn register_stream(&self, peer_addr: SocketAddr, sender: mpsc::UnboundedSender<Bytes>) {
        let mut streams = self.streams.write().await;
        streams.insert(peer_addr, sender);
    }

    /// Unregister a stream
    pub async fn unregister_stream(&self, peer_addr: SocketAddr) {
        let mut streams = self.streams.write().await;
        streams.remove(&peer_addr);
    }

    /// Route a packet to the appropriate stream
    pub async fn route_packet(&self, peer_addr: SocketAddr, data: Bytes) -> Result<bool> {
        let streams = self.streams.read().await;
        if let Some(sender) = streams.get(&peer_addr) {
            // Send packet to the stream's channel
            if sender.send(data).is_ok() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Check if a peer has an active stream
    pub async fn has_stream(&self, peer_addr: &SocketAddr) -> bool {
        let streams = self.streams.read().await;
        streams.contains_key(peer_addr)
    }

    /// Get the number of active streams
    pub async fn stream_count(&self) -> usize {
        let streams = self.streams.read().await;
        streams.len()
    }
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}
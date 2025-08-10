//! KCP listener for accepting incoming connections

use crate::async_kcp::stream::KcpStream;
use crate::common::*;
use crate::config::KcpConfig;
use crate::error::{ConnectionError, KcpError, Result};

use bytes::Bytes;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, RwLock};
use tokio::time::{Duration, Instant};
use tracing::{debug, error, info, trace, warn};

/// Incoming connection information
#[derive(Debug, Clone)]
pub struct IncomingConnection {
    pub peer_addr: SocketAddr,
    pub conv: ConvId,
    pub handshake_data: Bytes,
}

/// KCP listener for accepting incoming connections
pub struct KcpListener {
    socket: Arc<UdpSocket>,
    config: KcpConfig,
    local_addr: SocketAddr,

    // Connection management
    pending_connections: Arc<RwLock<HashMap<SocketAddr, IncomingConnection>>>,
    connection_queue: mpsc::UnboundedReceiver<IncomingConnection>,
    connection_sender: mpsc::UnboundedSender<IncomingConnection>,

    // Background task
    listen_task: Option<tokio::task::JoinHandle<()>>,
}

impl KcpListener {
    /// Bind to the specified address
    pub async fn bind(addr: SocketAddr, config: KcpConfig) -> Result<Self> {
        let socket = UdpSocket::bind(addr).await.map_err(KcpError::Io)?;

        let local_addr = socket.local_addr().map_err(KcpError::Io)?;

        let (connection_sender, connection_queue) = mpsc::unbounded_channel();

        let mut listener = Self {
            socket: Arc::new(socket),
            config,
            local_addr,
            pending_connections: Arc::new(RwLock::new(HashMap::new())),
            connection_queue,
            connection_sender,
            listen_task: None,
        };

        listener.start_listening().await?;

        info!(addr = %local_addr, "KCP listener started");
        Ok(listener)
    }

    /// Accept an incoming connection
    pub async fn accept(&mut self) -> Result<(KcpStream, SocketAddr)> {
        loop {
            if let Some(incoming) = self.connection_queue.recv().await {
                match self.create_stream_for_connection(incoming).await {
                    Ok(stream) => {
                        return Ok((stream.0, stream.1));
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to create stream for incoming connection");
                        continue;
                    }
                }
            } else {
                return Err(KcpError::connection(ConnectionError::Closed));
            }
        }
    }

    /// Get the local address
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Get current number of pending connections
    pub async fn pending_count(&self) -> usize {
        self.pending_connections.read().await.len()
    }

    /// Close the listener
    pub async fn close(&mut self) -> Result<()> {
        if let Some(task) = self.listen_task.take() {
            task.abort();
        }

        info!(addr = %self.local_addr, "KCP listener closed");
        Ok(())
    }

    /// Start the background listening task
    async fn start_listening(&mut self) -> Result<()> {
        let socket = self.socket.clone();
        let pending_connections = self.pending_connections.clone();
        let connection_sender = self.connection_sender.clone();
        let handshake_timeout = self.config.connect_timeout;

        self.listen_task = Some(tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            let mut cleanup_interval = tokio::time::interval(Duration::from_secs(30));

            loop {
                tokio::select! {
                    // Receive incoming packets
                    recv_result = socket.recv_from(&mut buf) => {
                        match recv_result {
                            Ok((size, peer_addr)) => {
                                let data = Bytes::copy_from_slice(&buf[..size]);

                                if let Err(e) = Self::handle_incoming_packet(
                                    data,
                                    peer_addr,
                                    &pending_connections,
                                    &connection_sender,
                                ).await {
                                    trace!(
                                        peer = %peer_addr,
                                        error = %e,
                                        "Failed to handle incoming packet"
                                    );
                                }
                            }
                            Err(e) => {
                                error!(error = %e, "UDP receive failed");
                                break;
                            }
                        }
                    }

                    // Cleanup expired pending connections
                    _ = cleanup_interval.tick() => {
                        Self::cleanup_pending_connections(&pending_connections, handshake_timeout).await;
                    }
                }
            }
        }));

        Ok(())
    }

    /// Handle incoming packet
    async fn handle_incoming_packet(
        data: Bytes,
        peer_addr: SocketAddr,
        pending_connections: &Arc<RwLock<HashMap<SocketAddr, IncomingConnection>>>,
        connection_sender: &mpsc::UnboundedSender<IncomingConnection>,
    ) -> Result<()> {
        // Check if this is a handshake packet
        if data.len() >= 13 && &data[..13] == b"KCP_HANDSHAKE" {
            let mut pending = pending_connections.write().await;

            if let std::collections::hash_map::Entry::Vacant(e) = pending.entry(peer_addr) {
                // New connection attempt
                let conv = Self::generate_conversation_id();
                let incoming = IncomingConnection {
                    peer_addr,
                    conv,
                    handshake_data: data.clone(),
                };

                e.insert(incoming.clone());

                // Notify about new connection
                if connection_sender.send(incoming).is_err() {
                    warn!("Connection queue is full or closed");
                }

                debug!(peer = %peer_addr, conv = conv, "New handshake received");
            }

            return Ok(());
        }

        // Check if this is a KCP packet from known peer
        if data.len() >= KcpHeader::SIZE {
            let mut data_clone = data.clone();
            if let Some(header) = KcpHeader::decode(&mut data_clone) {
                let pending = pending_connections.read().await;

                if let Some(incoming) = pending.get(&peer_addr) {
                    if header.conv == incoming.conv {
                        // This is a valid KCP packet for a pending connection
                        trace!(
                            peer = %peer_addr,
                            conv = header.conv,
                            cmd = header.cmd,
                            "KCP packet from pending connection"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Create a stream for an incoming connection
    async fn create_stream_for_connection(
        &self,
        incoming: IncomingConnection,
    ) -> Result<(KcpStream, SocketAddr)> {
        // Remove from pending connections
        {
            let mut pending = self.pending_connections.write().await;
            pending.remove(&incoming.peer_addr);
        }

        // Create a new UDP socket for this connection
        let new_socket = UdpSocket::bind("0.0.0.0:0").await.map_err(KcpError::Io)?;

        new_socket
            .connect(incoming.peer_addr)
            .await
            .map_err(KcpError::Io)?;

        // Send handshake response
        new_socket
            .send(&incoming.handshake_data)
            .await
            .map_err(KcpError::Io)?;

        // Create KCP stream
        let stream = KcpStream::new_with_socket(
            new_socket,
            incoming.peer_addr,
            self.config.clone(),
            false, // This is server side
        )
        .await?;

        info!(
            peer = %incoming.peer_addr,
            conv = incoming.conv,
            "Connection accepted"
        );

        Ok((stream, incoming.peer_addr))
    }

    /// Generate a unique conversation ID for new connections
    fn generate_conversation_id() -> ConvId {
        // Generate server-side conversation ID (MSB clear)
        rand::random::<u32>() & 0x7FFFFFFF
    }

    /// Clean up expired pending connections
    async fn cleanup_pending_connections(
        pending_connections: &Arc<RwLock<HashMap<SocketAddr, IncomingConnection>>>,
        _timeout: Duration,
    ) {
        let mut pending = pending_connections.write().await;
        let _now = Instant::now();

        // Note: In a real implementation, we would track when each connection was added
        // For simplicity, we'll just clean up all connections older than timeout
        let initial_count = pending.len();

        // For now, just clear all if we have too many pending
        if pending.len() > 1000 {
            pending.clear();
            debug!(
                cleared = initial_count,
                "Cleared pending connections due to limit"
            );
        }
    }
}

impl Drop for KcpListener {
    fn drop(&mut self) {
        if let Some(task) = self.listen_task.take() {
            task.abort();
        }
    }
}

// Make KcpListener safe to send between threads
unsafe impl Send for KcpListener {}
unsafe impl Sync for KcpListener {}

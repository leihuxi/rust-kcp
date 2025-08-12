//! KCP listener for accepting incoming connections

use crate::async_kcp::stream::KcpStream;
use crate::common::*;
use crate::config::KcpConfig;
use crate::error::{ConnectionError, KcpError, Result};

use bytes::Bytes;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::{Duration, Instant};
use tracing::{debug, error, info, trace, warn};

/// Simple stream handle for packet routing
struct StreamHandle {
    engine: Arc<Mutex<crate::async_kcp::engine::KcpEngine>>,
}

impl StreamHandle {
    async fn input_packet(&self, data: Bytes) -> Result<()> {
        let mut engine = self.engine.lock().await;
        engine.input(data).await?;
        engine.update().await?;
        Ok(())
    }
}

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
    accepting_connections: Arc<RwLock<HashSet<SocketAddr>>>, // Track connections being accepted
    active_connections: Arc<RwLock<HashSet<SocketAddr>>>,    // Track active established connections
    active_streams: Arc<RwLock<HashMap<SocketAddr, StreamHandle>>>, // Active streams for packet routing
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
            accepting_connections: Arc::new(RwLock::new(HashSet::new())),
            active_connections: Arc::new(RwLock::new(HashSet::new())),
            active_streams: Arc::new(RwLock::new(HashMap::new())),
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
                // Mark as accepting to prevent duplicate processing
                {
                    let mut accepting = self.accepting_connections.write().await;
                    let active = self.active_connections.read().await;

                    // Double-check that this peer isn't already active
                    if active.contains(&incoming.peer_addr) {
                        info!(peer = %incoming.peer_addr, "Ignoring connection attempt from active peer");
                        continue;
                    }

                    accepting.insert(incoming.peer_addr);
                }

                match self.create_stream_for_connection(incoming.clone()).await {
                    Ok(stream) => {
                        // Remove from accepting set (active marking done in create_stream_for_connection)
                        {
                            let mut accepting = self.accepting_connections.write().await;
                            accepting.remove(&incoming.peer_addr);
                        }
                        return Ok((stream.0, stream.1));
                    }
                    Err(e) => {
                        // Remove from accepting set on error
                        {
                            let mut accepting = self.accepting_connections.write().await;
                            accepting.remove(&incoming.peer_addr);
                        }
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

    /// Remove a connection from active tracking (called when stream is dropped)
    pub async fn remove_active_connection(&self, peer_addr: SocketAddr) {
        let mut active = self.active_connections.write().await;
        active.remove(&peer_addr);
        debug!(peer = %peer_addr, "Removed active connection");
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
        let accepting_connections = self.accepting_connections.clone();
        let active_connections = self.active_connections.clone();
        let active_streams = self.active_streams.clone();
        let connection_sender = self.connection_sender.clone();
        let handshake_timeout = self.config.connect_timeout;

        self.listen_task = Some(tokio::spawn(async move {
            info!("Listener background task started, waiting for packets...");
            let mut buf = vec![0u8; 65536];
            let mut cleanup_interval = tokio::time::interval(Duration::from_secs(30));

            loop {
                tokio::select! {
                    // Receive incoming packets
                    recv_result = socket.recv_from(&mut buf) => {
                        trace!("Listener received packet result: {:?}", recv_result.as_ref().map(|(size, addr)| (*size, *addr)));
                        match recv_result {
                            Ok((size, peer_addr)) => {
                                let data = Bytes::copy_from_slice(&buf[..size]);
                                trace!("Received {} bytes from {}: {:02x?}", size, peer_addr, &data[..size.min(16)]);

                                // First check if this is from an active stream
                                let streams = active_streams.read().await;
                                let stream_count = streams.len();
                                trace!(peer = %peer_addr, stream_count = stream_count, "Checking for active stream");

                                if let Some(stream) = streams.get(&peer_addr) {
                                    // Route packet to active stream
                                    trace!(peer = %peer_addr, "Routing packet to active stream");
                                    if let Err(e) = stream.input_packet(data).await {
                                        trace!(
                                            peer = %peer_addr,
                                            error = %e,
                                            "Failed to input packet to stream"
                                        );
                                    } else {
                                        trace!(peer = %peer_addr, "Successfully routed packet to stream");
                                    }
                                    continue;
                                } else {
                                    trace!(peer = %peer_addr, "No active stream found for this peer");
                                }
                                drop(streams);

                                // Not an active stream, handle as potential new connection
                                if let Err(e) = Self::handle_incoming_packet(
                                    data,
                                    peer_addr,
                                    &pending_connections,
                                    &accepting_connections,
                                    &active_connections,
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
        accepting_connections: &Arc<RwLock<HashSet<SocketAddr>>>,
        active_connections: &Arc<RwLock<HashSet<SocketAddr>>>,
        connection_sender: &mpsc::UnboundedSender<IncomingConnection>,
    ) -> Result<()> {
        // Check if this is a KCP packet (standard KCP protocol)
        if data.len() >= KcpHeader::SIZE {
            let mut data_clone = data.clone();
            if let Some(header) = KcpHeader::decode(&mut data_clone) {
                // Use read locks to check all connection states
                let pending = pending_connections.read().await;
                let accepting = accepting_connections.read().await;
                let active = active_connections.read().await;

                // Ignore packets from peers that already have active connections
                if active.contains(&peer_addr) {
                    trace!(
                        peer = %peer_addr,
                        conv = header.conv,
                        "Packet from active connection, ignoring (handled by stream)"
                    );
                    return Ok(());
                }

                // Check if already accepting or pending this connection
                if accepting.contains(&peer_addr) {
                    trace!(
                        peer = %peer_addr,
                        conv = header.conv,
                        "Connection is being accepted, packet will be handled by stream once created"
                    );
                    return Ok(());
                }

                // If connection is pending, we might want to process additional packets
                // but for now, we'll treat them as retransmissions to avoid duplicate connections
                if pending.contains_key(&peer_addr) {
                    trace!(
                        peer = %peer_addr,
                        conv = header.conv,
                        "Connection already pending, may be retransmission or additional data packet"
                    );
                    return Ok(());
                }

                // Release read locks before acquiring write lock
                drop(active);
                drop(accepting);
                drop(pending);

                // This is truly a new connection - first KCP packet from this peer
                debug!(
                    peer = %peer_addr,
                    conv = header.conv,
                    "First KCP packet from new peer"
                );

                let incoming = IncomingConnection {
                    peer_addr,
                    conv: header.conv, // Use conversation ID from client
                    handshake_data: data.clone(),
                };

                // Insert into pending FIRST to prevent duplicates from racing
                let mut pending = pending_connections.write().await;
                pending.insert(peer_addr, incoming.clone());
                drop(pending);

                // Notify about new connection
                if connection_sender.send(incoming).is_err() {
                    warn!("Connection queue is full or closed");
                    // Remove from pending if we couldn't queue it
                    let mut pending = pending_connections.write().await;
                    pending.remove(&peer_addr);
                } else {
                    debug!(
                        peer = %peer_addr,
                        conv = header.conv,
                        "New KCP connection queued for accept"
                    );
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
        // Create server stream using the shared socket
        let stream = KcpStream::new_server_stream(
            self.socket.clone(),
            incoming.peer_addr,
            incoming.conv, // Use the conversation ID from client
            self.config.clone(),
            incoming.handshake_data, // Pass the initial packet
        )
        .await?;

        // Create a handle for packet routing
        let handle = StreamHandle {
            engine: stream.engine.clone(),
        };

        // Mark connection as active and save the handle
        {
            let mut pending = self.pending_connections.write().await;
            let mut active = self.active_connections.write().await;
            let mut streams = self.active_streams.write().await;

            pending.remove(&incoming.peer_addr);
            active.insert(incoming.peer_addr);
            streams.insert(incoming.peer_addr, handle);
        }

        info!(
            peer = %incoming.peer_addr,
            conv = incoming.conv,
            "Connection accepted and registered for packet routing"
        );

        Ok((stream, incoming.peer_addr))
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

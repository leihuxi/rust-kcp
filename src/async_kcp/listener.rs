//! KCP listener for accepting incoming connections

use crate::async_kcp::stream::KcpStream;
use crate::common::*;
use crate::config::KcpConfig;
use crate::error::{ConnectionError, KcpError, Result};
use crate::transport::{Addr, Transport, UdpTransport};

use bytes::Bytes;
use dashmap::DashMap;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::time::{Duration, Instant};
use tracing::{debug, error, info, trace, warn};

/// Lock-free stream handle for packet routing — just a channel sender.
struct StreamHandle {
    input_tx: mpsc::Sender<Bytes>,
}

/// Incoming connection information
#[derive(Debug, Clone)]
pub struct IncomingConnection<A: Addr> {
    pub peer_addr: A,
    pub conv: ConvId,
    pub handshake_data: Bytes,
    created_at: Instant,
}

/// Connection management state (consolidated into one struct behind a single lock)
struct ConnectionState<A: Addr> {
    pending: HashMap<A, IncomingConnection<A>>,
    accepting: std::collections::HashSet<A>,
    active: std::collections::HashSet<A>,
}

impl<A: Addr> ConnectionState<A> {
    fn new() -> Self {
        Self {
            pending: HashMap::new(),
            accepting: std::collections::HashSet::new(),
            active: std::collections::HashSet::new(),
        }
    }
}

/// KCP listener for accepting incoming connections
pub struct KcpListener<T: Transport = UdpTransport> {
    transport: Arc<T>,
    config: KcpConfig,
    local_addr: T::Addr,

    // Connection management (single lock for pending/accepting/active)
    conn_state: Arc<RwLock<ConnectionState<T::Addr>>>,
    // Active streams for packet routing (lock-free concurrent map, accessed on every packet)
    active_streams: Arc<DashMap<T::Addr, StreamHandle>>,

    connection_queue: mpsc::UnboundedReceiver<IncomingConnection<T::Addr>>,
    connection_sender: mpsc::UnboundedSender<IncomingConnection<T::Addr>>,

    // Background task
    listen_task: Option<tokio::task::JoinHandle<()>>,
}

// --- UDP-specific convenience methods ---

impl KcpListener<UdpTransport> {
    /// Bind to the specified address
    pub async fn bind(addr: SocketAddr, config: KcpConfig) -> Result<Self> {
        let transport = UdpTransport::bind(addr)
            .await
            .map_err(KcpError::Io)?;
        Self::with_transport(Arc::new(transport), config).await
    }
}

// --- Generic methods for any Transport ---

impl<T: Transport> KcpListener<T> {
    /// Create a listener using a custom [`Transport`].
    pub async fn with_transport(transport: Arc<T>, config: KcpConfig) -> Result<Self> {
        let local_addr = transport.local_addr().map_err(KcpError::Io)?;

        let (connection_sender, connection_queue) = mpsc::unbounded_channel();

        let mut listener = Self {
            transport,
            config,
            local_addr: local_addr.clone(),
            conn_state: Arc::new(RwLock::new(ConnectionState::new())),
            active_streams: Arc::new(DashMap::new()),
            connection_queue,
            connection_sender,
            listen_task: None,
        };

        listener.start_listening().await?;

        info!(addr = %local_addr, "KCP listener started");
        Ok(listener)
    }

    /// Accept an incoming connection
    pub async fn accept(&mut self) -> Result<(KcpStream<T>, T::Addr)> {
        loop {
            if let Some(incoming) = self.connection_queue.recv().await {
                // Save peer_addr for cleanup; move incoming by value below
                let peer_addr = incoming.peer_addr.clone();

                // Mark as accepting to prevent duplicate processing
                {
                    let mut state = self.conn_state.write().await;

                    if state.active.contains(&peer_addr) {
                        info!(peer = %peer_addr, "Ignoring connection attempt from active peer");
                        continue;
                    }

                    state.accepting.insert(peer_addr.clone());
                }

                // Move incoming (avoids cloning Bytes + entire IncomingConnection)
                let result = self.create_stream_for_connection(incoming).await;
                {
                    let mut state = self.conn_state.write().await;
                    state.accepting.remove(&peer_addr);
                }
                match result {
                    Ok(stream) => return Ok(stream),
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
    pub fn local_addr(&self) -> &T::Addr {
        &self.local_addr
    }

    /// Get current number of pending connections
    pub async fn pending_count(&self) -> usize {
        self.conn_state.read().await.pending.len()
    }

    /// Remove a connection from active tracking (called when stream is dropped)
    pub async fn remove_active_connection(&self, peer_addr: &T::Addr) {
        let mut state = self.conn_state.write().await;
        state.active.remove(peer_addr);
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
        let transport = self.transport.clone();
        let conn_state = self.conn_state.clone();
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
                    recv_result = transport.recv_from(&mut buf) => {
                        match recv_result {
                            Ok((size, peer_addr)) => {
                                let data = Bytes::copy_from_slice(&buf[..size]);
                                trace!("Received {} bytes from {}", size, peer_addr);

                                // First check if this is from an active stream (lock-free lookup)
                                if let Some(handle) = active_streams.get(&peer_addr) {
                                    let _ = handle.input_tx.try_send(data);
                                    continue;
                                }

                                // Not an active stream, handle as potential new connection
                                if let Err(e) = Self::handle_incoming_packet(
                                    data,
                                    &peer_addr,
                                    &conn_state,
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

                    // Periodic cleanup
                    _ = cleanup_interval.tick() => {
                        Self::cleanup_dead_streams(&active_streams, &conn_state).await;
                        Self::cleanup_pending_connections(&conn_state, handshake_timeout).await;
                    }
                }
            }
        }));

        Ok(())
    }

    /// Handle incoming packet
    async fn handle_incoming_packet(
        data: Bytes,
        peer_addr: &T::Addr,
        conn_state: &Arc<RwLock<ConnectionState<T::Addr>>>,
        connection_sender: &mpsc::UnboundedSender<IncomingConnection<T::Addr>>,
    ) -> Result<()> {
        if data.len() >= KcpHeader::SIZE {
            let mut data_clone = data.clone();
            if let Some(header) = KcpHeader::decode(&mut data_clone) {
                let state = conn_state.read().await;

                if state.active.contains(peer_addr) {
                    trace!(peer = %peer_addr, "Packet from active connection, ignoring");
                    return Ok(());
                }

                if state.accepting.contains(peer_addr) {
                    trace!(peer = %peer_addr, "Connection is being accepted");
                    return Ok(());
                }

                if state.pending.contains_key(peer_addr) {
                    trace!(peer = %peer_addr, "Connection already pending");
                    return Ok(());
                }

                drop(state);

                debug!(
                    peer = %peer_addr,
                    conv = header.conv,
                    "First KCP packet from new peer"
                );

                let incoming = IncomingConnection {
                    peer_addr: peer_addr.clone(),
                    conv: header.conv,
                    handshake_data: data.clone(),
                    created_at: Instant::now(),
                };

                let mut state = conn_state.write().await;
                state.pending.insert(peer_addr.clone(), incoming.clone());
                drop(state);

                if connection_sender.send(incoming).is_err() {
                    warn!("Connection queue is full or closed");
                    let mut state = conn_state.write().await;
                    state.pending.remove(peer_addr);
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
        incoming: IncomingConnection<T::Addr>,
    ) -> Result<(KcpStream<T>, T::Addr)> {
        let peer_addr = incoming.peer_addr;
        let stream = KcpStream::new_server_stream(
            self.transport.clone(),
            peer_addr.clone(),
            incoming.conv,
            self.config.clone(),
            incoming.handshake_data,
        )
        .await?;

        // Create a lightweight handle for packet routing
        let handle = StreamHandle {
            input_tx: stream.input_sender(),
        };

        // Mark connection as active and save the handle
        {
            let mut state = self.conn_state.write().await;
            state.pending.remove(&peer_addr);
            state.active.insert(peer_addr.clone());
        }

        info!(
            peer = %peer_addr,
            conv = incoming.conv,
            "Connection accepted and registered for packet routing"
        );

        // Use final move for DashMap insert + return (avoids one clone)
        self.active_streams.insert(peer_addr.clone(), handle);
        Ok((stream, peer_addr))
    }

    /// Remove streams whose input channel has closed (actor exited -> receiver dropped).
    async fn cleanup_dead_streams(
        active_streams: &Arc<DashMap<T::Addr, StreamHandle>>,
        conn_state: &Arc<RwLock<ConnectionState<T::Addr>>>,
    ) {
        let dead_addrs: Vec<T::Addr> = active_streams
            .iter()
            .filter(|entry| entry.value().input_tx.is_closed())
            .map(|entry| entry.key().clone())
            .collect();

        if !dead_addrs.is_empty() {
            let mut state = conn_state.write().await;
            for addr in &dead_addrs {
                active_streams.remove(addr);
                state.active.remove(addr);
            }
            debug!(
                removed = dead_addrs.len(),
                remaining = active_streams.len(),
                "Cleaned up dead streams"
            );
        }
    }

    /// Clean up expired pending connections
    async fn cleanup_pending_connections(
        conn_state: &Arc<RwLock<ConnectionState<T::Addr>>>,
        timeout: Duration,
    ) {
        let mut state = conn_state.write().await;
        let now = Instant::now();

        let before = state.pending.len();
        state
            .pending
            .retain(|_addr, conn| now.duration_since(conn.created_at) < timeout);

        let removed = before - state.pending.len();
        if removed > 0 {
            debug!(
                removed,
                remaining = state.pending.len(),
                "Cleaned up expired pending connections"
            );
        }
    }
}

impl<T: Transport> Drop for KcpListener<T> {
    fn drop(&mut self) {
        if let Some(task) = self.listen_task.take() {
            task.abort();
        }
    }
}

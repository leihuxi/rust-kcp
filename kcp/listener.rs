//! KCP listener for accepting incoming connections

use crate::stream::KcpStream;
use crate::common::*;
use crate::config::KcpConfig;
use crate::error::{ConnectionError, KcpError, Result};
use crate::transport::{Addr, Transport, UdpTransport};

use bytes::Bytes;
use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::time::{Duration, Instant};
use tracing::{debug, error, info, trace, warn};

/// Routing key: a session is identified by peer address *and* conversation
/// id, so a new session from the same source port (different conv) is never
/// mistaken for — or blocked by — a stale one (issue #7).
type SessionKey<A> = (A, ConvId);

/// Lock-free stream handle for packet routing — just a channel sender.
struct StreamHandle {
    input_tx: mpsc::Sender<Bytes>,
    /// For a primary entry `(addr, server_conv)` whose handshake arrived with
    /// `conv == 0`: true while the `(addr, 0)` alias still exists. Cleared
    /// (and the alias removed) once the peer sends its first packet under
    /// `server_conv`, so a later `conv == 0` handshake from the same address
    /// opens a fresh session instead of being absorbed by this one.
    has_zero_alias: AtomicBool,
    /// Set on the `(addr, 0)` alias entry: the server conv it stands in for.
    alias_of: Option<ConvId>,
}

/// Read the conv field (first 4 bytes, little-endian) of a KCP datagram.
fn wire_conv(data: &[u8]) -> Option<ConvId> {
    data.get(..4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Incoming connection information
#[derive(Debug, Clone)]
pub struct IncomingConnection<A: Addr> {
    pub peer_addr: A,
    /// Conv the server uses for outgoing traffic. Always non-zero. If the
    /// client opened the connection with `conv == 0` (tokio_kcp handshake
    /// convention), the listener allocates a fresh random conv here.
    pub conv: ConvId,
    /// Conv observed on the wire in the first packet. Needed so the engine
    /// can parse that initial packet before we switch to the server-side conv.
    client_conv: ConvId,
    pub handshake_data: Bytes,
    created_at: Instant,
}

/// Connection management state (consolidated into one struct behind a single lock)
///
/// `pending` / `accepting` are keyed by the conv seen on the wire in the
/// first packet; `active` holds every routing key registered in
/// `active_streams` (the primary `(addr, server_conv)` plus any `(addr, 0)`
/// alias).
struct ConnectionState<A: Addr> {
    pending: HashMap<SessionKey<A>, IncomingConnection<A>>,
    accepting: HashSet<SessionKey<A>>,
    active: HashSet<SessionKey<A>>,
}

impl<A: Addr> ConnectionState<A> {
    fn new() -> Self {
        Self {
            pending: HashMap::new(),
            accepting: HashSet::new(),
            active: HashSet::new(),
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
    active_streams: Arc<DashMap<SessionKey<T::Addr>, StreamHandle>>,

    connection_queue: mpsc::Receiver<IncomingConnection<T::Addr>>,
    connection_sender: mpsc::Sender<IncomingConnection<T::Addr>>,

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
        config.validate()?;
        let local_addr = transport.local_addr().map_err(KcpError::Io)?;

        let (connection_sender, connection_queue) = mpsc::channel(config.max_pending_connections);

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
                // Save the key for cleanup; move incoming by value below
                let key = (incoming.peer_addr.clone(), incoming.client_conv);

                // Mark as accepting to prevent duplicate processing
                {
                    let mut state = self.conn_state.write().await;

                    if state.active.contains(&key) {
                        info!(
                            peer = %key.0,
                            conv = key.1,
                            "Ignoring connection attempt for an active session"
                        );
                        state.pending.remove(&key);
                        continue;
                    }

                    state.accepting.insert(key.clone());
                }

                // Move incoming (avoids cloning Bytes + entire IncomingConnection)
                let result = self.create_stream_for_connection(incoming).await;
                {
                    let mut state = self.conn_state.write().await;
                    state.accepting.remove(&key);
                    // On failure, drop the pending entry too so the peer can retry.
                    state.pending.remove(&key);
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

    /// Remove every session from `peer_addr` from active tracking and packet
    /// routing. Dead sessions are also removed automatically, so calling this
    /// is optional.
    pub async fn remove_active_connection(&self, peer_addr: &T::Addr) {
        let mut state = self.conn_state.write().await;
        state.active.retain(|(addr, _)| addr != peer_addr);
        self.active_streams.retain(|(addr, _), _| addr != peer_addr);
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
        let max_pending = self.config.max_pending_connections;
        let cleanup_dur = self.config.cleanup_interval;

        self.listen_task = Some(tokio::spawn(async move {
            info!("Listener background task started, waiting for packets...");
            let mut buf = vec![0u8; 65536];
            let mut cleanup_interval = tokio::time::interval(cleanup_dur);

            loop {
                tokio::select! {
                    // Receive incoming packets
                    recv_result = transport.recv_from(&mut buf) => {
                        match recv_result {
                            Ok((size, peer_addr)) => {
                                let data = Bytes::copy_from_slice(&buf[..size]);
                                trace!("Received {} bytes from {}", size, peer_addr);

                                // Route to an active session (lock-free lookup)
                                if let Some(conv) = wire_conv(&data) {
                                    if Self::route_to_active(
                                        &active_streams,
                                        &conn_state,
                                        &peer_addr,
                                        conv,
                                        &data,
                                    ).await {
                                        continue;
                                    }
                                }

                                // Not an active stream, handle as potential new connection
                                if let Err(e) = Self::handle_incoming_packet(
                                    data,
                                    &peer_addr,
                                    &conn_state,
                                    &connection_sender,
                                    max_pending,
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
        connection_sender: &mpsc::Sender<IncomingConnection<T::Addr>>,
        max_pending: usize,
    ) -> Result<()> {
        if data.len() >= KcpHeader::SIZE {
            let mut data_clone = data.clone();
            if let Some(header) = KcpHeader::decode(&mut data_clone) {
                // Cheap filter: only a valid KCP command byte may open a new
                // connection. Rejects port scans / stray UDP from spawning
                // phantom pending entries (and from reaching the engine's
                // segment parser via accept()).
                if !matches!(
                    header.cmd,
                    constants::IKCP_CMD_PUSH
                        | constants::IKCP_CMD_ACK
                        | constants::IKCP_CMD_WASK
                        | constants::IKCP_CMD_WINS
                ) {
                    trace!(peer = %peer_addr, cmd = header.cmd, "Ignoring non-KCP datagram");
                    return Ok(());
                }

                let key = (peer_addr.clone(), header.conv);
                let state = conn_state.read().await;

                if state.active.contains(&key) {
                    trace!(peer = %peer_addr, conv = header.conv, "Packet from active session, ignoring");
                    return Ok(());
                }

                if state.accepting.contains(&key) {
                    trace!(peer = %peer_addr, "Connection is being accepted");
                    return Ok(());
                }

                if state.pending.contains_key(&key) {
                    trace!(peer = %peer_addr, "Connection already pending");
                    return Ok(());
                }

                if state.pending.len() >= max_pending {
                    warn!(
                        peer = %peer_addr,
                        pending = state.pending.len(),
                        max = max_pending,
                        "Rejecting new connection: pending limit reached"
                    );
                    return Ok(());
                }

                drop(state);

                // tokio_kcp handshake: a client that calls connect_with_conv(_, 0, _)
                // expects the server to reply with a new non-zero conv, which lets
                // the client's Kcp::input() clear its `waiting_conv` flag. If we echo
                // back conv=0 verbatim, the client refuses to send any further data.
                let client_conv = header.conv;
                let server_conv = if client_conv == 0 {
                    crate::common::random_conv_id()
                } else {
                    client_conv
                };

                debug!(
                    peer = %peer_addr,
                    client_conv,
                    server_conv,
                    "First KCP packet from new peer"
                );

                let incoming = IncomingConnection {
                    peer_addr: peer_addr.clone(),
                    conv: server_conv,
                    client_conv,
                    handshake_data: data.clone(),
                    created_at: Instant::now(),
                };

                let mut state = conn_state.write().await;
                state.pending.insert(key.clone(), incoming.clone());
                drop(state);

                if connection_sender.try_send(incoming).is_err() {
                    warn!("Connection queue is full or closed");
                    let mut state = conn_state.write().await;
                    state.pending.remove(&key);
                } else {
                    debug!(
                        peer = %peer_addr,
                        client_conv,
                        server_conv,
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
            incoming.client_conv,
            incoming.conv,
            self.config.clone(),
            incoming.handshake_data,
        )
        .await?;

        let client_conv = incoming.client_conv;
        let server_conv = incoming.conv;
        let primary = (peer_addr.clone(), server_conv);
        // A conv=0 handshake keeps an `(addr, 0)` alias until the peer switches
        // to `server_conv`, so its handshake retransmits are absorbed by this
        // session instead of opening duplicate sessions.
        let alias = (client_conv != server_conv).then(|| (peer_addr.clone(), client_conv));

        // Register routing before marking active, so a packet that sees the key
        // in `active` always finds the handle too.
        self.active_streams.insert(
            primary.clone(),
            StreamHandle {
                input_tx: stream.input_sender(),
                has_zero_alias: AtomicBool::new(alias.is_some()),
                alias_of: None,
            },
        );
        if let Some(alias) = &alias {
            self.active_streams.insert(
                alias.clone(),
                StreamHandle {
                    input_tx: stream.input_sender(),
                    has_zero_alias: AtomicBool::new(false),
                    alias_of: Some(server_conv),
                },
            );
        }
        {
            let mut state = self.conn_state.write().await;
            state.pending.remove(&(peer_addr.clone(), client_conv));
            state.active.insert(primary);
            if let Some(alias) = alias {
                state.active.insert(alias);
            }
        }

        info!(
            peer = %peer_addr,
            conv = server_conv,
            "Connection accepted and registered for packet routing"
        );

        Ok((stream, peer_addr))
    }

    /// Route a datagram to the active session `(peer_addr, conv)`.
    ///
    /// Returns `false` when there is no live session for that key — including
    /// when the entry belongs to a stream whose actor has exited, which is
    /// removed on the spot rather than waiting for `cleanup_interval`, so the
    /// caller can treat the packet as a new connection.
    async fn route_to_active(
        active_streams: &Arc<DashMap<SessionKey<T::Addr>, StreamHandle>>,
        conn_state: &Arc<RwLock<ConnectionState<T::Addr>>>,
        peer_addr: &T::Addr,
        conv: ConvId,
        data: &Bytes,
    ) -> bool {
        let key = (peer_addr.clone(), conv);
        // Never hold a DashMap guard across another map operation (same-shard
        // deadlock) or an await.
        let (alive, drop_alias) = match active_streams.get(&key) {
            None => return false,
            Some(handle) => {
                if handle.input_tx.is_closed() {
                    (false, false)
                } else {
                    let _ = handle.input_tx.try_send(data.clone());
                    (true, handle.has_zero_alias.swap(false, Ordering::Relaxed))
                }
            }
        };

        if alive {
            if drop_alias {
                // Peer has switched to the server conv: retire the conv=0 alias.
                let alias = (peer_addr.clone(), 0);
                if active_streams
                    .remove_if(&alias, |_, h| h.alias_of == Some(conv))
                    .is_some()
                {
                    conn_state.write().await.active.remove(&alias);
                    trace!(peer = %peer_addr, conv, "Retired conv=0 handshake alias");
                }
            }
            return true;
        }

        // Dead session: unregister it (and its alias) now.
        let removed = active_streams.remove_if(&key, |_, h| h.input_tx.is_closed());
        let mut state = conn_state.write().await;
        state.active.remove(&key);
        if let Some((_, h)) = removed {
            if h.has_zero_alias.load(Ordering::Relaxed) {
                let alias = (peer_addr.clone(), 0);
                if active_streams
                    .remove_if(&alias, |_, a| a.alias_of == Some(conv))
                    .is_some()
                {
                    state.active.remove(&alias);
                }
            }
        }
        debug!(peer = %peer_addr, conv, "Removed dead session on new packet");
        false
    }

    /// Remove streams whose input channel has closed (actor exited -> receiver dropped).
    async fn cleanup_dead_streams(
        active_streams: &Arc<DashMap<SessionKey<T::Addr>, StreamHandle>>,
        conn_state: &Arc<RwLock<ConnectionState<T::Addr>>>,
    ) {
        let dead_addrs: Vec<SessionKey<T::Addr>> = active_streams
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

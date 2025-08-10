//! Connection management and multiplexing

use crate::async_kcp::engine::{KcpEngine, OutputFn};
use crate::common::*;
use crate::config::KcpConfig;
use crate::error::{ConnectionError, KcpError, Result};

use bytes::Bytes;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::{interval, Instant};
use tracing::{debug, error, info, trace};

/// Connection state
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    /// Connection is being established
    Connecting,
    /// Connection is active and ready for data transfer
    Connected,
    /// Connection is being closed gracefully
    Closing,
    /// Connection is closed
    Closed,
    /// Connection failed due to error
    Failed,
}

/// Represents a single KCP connection
pub struct KcpConnection {
    conv: ConvId,
    pub peer_addr: SocketAddr,
    engine: Arc<Mutex<KcpEngine>>,
    state: Arc<Mutex<ConnectionState>>,
    #[allow(dead_code)]
    config: KcpConfig,

    // Connection timing
    created_at: Instant,
    last_activity: Arc<Mutex<Instant>>,

    // Data channels
    #[allow(dead_code)]
    incoming_tx: mpsc::UnboundedSender<Bytes>,
    #[allow(dead_code)]
    outgoing_tx: mpsc::UnboundedSender<Bytes>,

    // Statistics
    stats: Arc<Mutex<KcpStats>>,
}

impl KcpConnection {
    /// Create a new connection
    pub fn new(
        conv: ConvId,
        peer_addr: SocketAddr,
        config: KcpConfig,
        output_fn: OutputFn,
    ) -> Self {
        let engine = KcpEngine::new(conv, config.clone());
        let engine = Arc::new(Mutex::new(engine));

        let (incoming_tx, _) = mpsc::unbounded_channel();
        let (outgoing_tx, _outgoing_rx) = mpsc::unbounded_channel();
        let now = Instant::now();

        // Set output function for the engine
        tokio::spawn({
            let engine = engine.clone();
            let output_fn = output_fn.clone();
            async move {
                let mut engine = engine.lock().await;
                engine.set_output(output_fn);
            }
        });

        Self {
            conv,
            peer_addr,
            engine,
            state: Arc::new(Mutex::new(ConnectionState::Connecting)),
            config,
            created_at: now,
            last_activity: Arc::new(Mutex::new(now)),
            incoming_tx,
            outgoing_tx,
            stats: Arc::new(Mutex::new(KcpStats::default())),
        }
    }

    /// Start the connection
    pub async fn start(&mut self) -> Result<()> {
        {
            let mut engine = self.engine.lock().await;
            engine.start().await?;
        }

        {
            let mut state = self.state.lock().await;
            *state = ConnectionState::Connected;
        }

        info!(conv = %self.conv, peer = %self.peer_addr, "Connection started");
        Ok(())
    }

    /// Send data through the connection
    pub async fn send(&self, data: Bytes) -> Result<()> {
        let state = self.state.lock().await;
        if *state != ConnectionState::Connected {
            return Err(KcpError::connection(ConnectionError::NotConnected));
        }

        // Send directly through engine for now
        let mut engine = self.engine.lock().await;
        engine.send(data).await?;

        self.update_activity().await;
        Ok(())
    }

    /// Receive data from the connection
    pub async fn recv(&mut self) -> Result<Option<Bytes>> {
        let mut engine = self.engine.lock().await;
        let result = engine.recv().await?;

        if result.is_some() {
            self.update_activity().await;
        }

        Ok(result)
    }

    /// Process incoming packet
    pub async fn input(&self, data: Bytes) -> Result<()> {
        let mut engine = self.engine.lock().await;
        engine.input(data).await?;

        self.update_activity().await;
        Ok(())
    }

    /// Update connection state
    pub async fn update(&self) -> Result<()> {
        let mut engine = self.engine.lock().await;

        // Note: For now, we'll handle outgoing data directly in send() method
        // TODO: Consider using a background task for outgoing data processing

        // Update KCP engine
        engine.update().await?;

        // Check if connection is dead
        if engine.is_dead() {
            let mut state = self.state.lock().await;
            *state = ConnectionState::Failed;
            return Err(KcpError::connection(ConnectionError::Lost));
        }

        // Update statistics
        {
            let mut stats = self.stats.lock().await;
            *stats = engine.stats().clone();
        }

        Ok(())
    }

    /// Close the connection
    pub async fn close(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        *state = ConnectionState::Closing;

        // Flush any remaining data
        {
            let mut engine = self.engine.lock().await;
            engine.flush().await?;
        }

        *state = ConnectionState::Closed;

        info!(conv = %self.conv, peer = %self.peer_addr, "Connection closed");
        Ok(())
    }

    /// Get connection information
    pub fn info(&self) -> ConnectionInfo {
        ConnectionInfo {
            conv: self.conv,
            peer_addr: self.peer_addr,
            created_at: self.created_at,
        }
    }

    /// Get current state
    pub async fn state(&self) -> ConnectionState {
        self.state.lock().await.clone()
    }

    /// Get connection statistics
    pub async fn stats(&self) -> KcpStats {
        self.stats.lock().await.clone()
    }

    /// Check if connection has been idle for too long
    pub async fn is_idle(&self, timeout: Duration) -> bool {
        let last_activity = *self.last_activity.lock().await;
        Instant::now().duration_since(last_activity) > timeout
    }

    /// Update last activity timestamp
    async fn update_activity(&self) {
        let mut last_activity = self.last_activity.lock().await;
        *last_activity = Instant::now();
    }
}

/// Connection information
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub conv: ConvId,
    pub peer_addr: SocketAddr,
    pub created_at: Instant,
}

/// Connection manager for handling multiple KCP connections
pub struct ConnectionManager {
    socket: Arc<UdpSocket>,
    config: KcpConfig,

    // Connection storage
    connections: Arc<RwLock<HashMap<ConvId, Arc<KcpConnection>>>>,
    addr_to_conv: Arc<RwLock<HashMap<SocketAddr, ConvId>>>,

    // Background tasks
    update_task: Option<tokio::task::JoinHandle<()>>,
    receive_task: Option<tokio::task::JoinHandle<()>>,
    cleanup_task: Option<tokio::task::JoinHandle<()>>,

    // Event channels
    new_connection_tx: mpsc::UnboundedSender<Arc<KcpConnection>>,
    new_connection_rx: mpsc::UnboundedReceiver<Arc<KcpConnection>>,
}

impl ConnectionManager {
    /// Create a new connection manager
    pub fn new(socket: UdpSocket, config: KcpConfig) -> Self {
        let (new_connection_tx, new_connection_rx) = mpsc::unbounded_channel();

        Self {
            socket: Arc::new(socket),
            config,
            connections: Arc::new(RwLock::new(HashMap::new())),
            addr_to_conv: Arc::new(RwLock::new(HashMap::new())),
            update_task: None,
            receive_task: None,
            cleanup_task: None,
            new_connection_tx,
            new_connection_rx,
        }
    }

    /// Start the connection manager
    pub async fn start(&mut self) -> Result<()> {
        self.start_background_tasks().await?;
        info!("Connection manager started");
        Ok(())
    }

    /// Create a new outgoing connection
    pub async fn connect(&self, peer_addr: SocketAddr, conv: ConvId) -> Result<Arc<KcpConnection>> {
        let socket = self.socket.clone();
        let output_fn: OutputFn = Arc::new(move |data: Bytes| {
            let socket = socket.clone();
            Box::pin(async move {
                socket
                    .send_to(&data, peer_addr)
                    .await
                    .map_err(KcpError::Io)?;
                Ok(())
            })
        });

        let mut connection = KcpConnection::new(conv, peer_addr, self.config.clone(), output_fn);
        connection.start().await?;

        let connection = Arc::new(connection);

        // Store connection
        {
            let mut connections = self.connections.write().await;
            connections.insert(conv, connection.clone());
        }

        {
            let mut addr_to_conv = self.addr_to_conv.write().await;
            addr_to_conv.insert(peer_addr, conv);
        }

        self.new_connection_tx
            .send(connection.clone())
            .map_err(|_| KcpError::connection(ConnectionError::Closed))?;

        Ok(connection)
    }

    /// Accept new connections
    pub async fn accept(&mut self) -> Result<Arc<KcpConnection>> {
        self.new_connection_rx
            .recv()
            .await
            .ok_or_else(|| KcpError::connection(ConnectionError::Closed))
    }

    /// Get connection by conversation ID
    pub async fn get_connection(&self, conv: ConvId) -> Option<Arc<KcpConnection>> {
        let connections = self.connections.read().await;
        connections.get(&conv).cloned()
    }

    /// Get connection by peer address
    pub async fn get_connection_by_addr(&self, addr: SocketAddr) -> Option<Arc<KcpConnection>> {
        let addr_to_conv = self.addr_to_conv.read().await;
        let conv = addr_to_conv.get(&addr)?;

        let connections = self.connections.read().await;
        connections.get(conv).cloned()
    }

    /// Remove a connection
    pub async fn remove_connection(&self, conv: ConvId) -> Result<()> {
        let connection = {
            let mut connections = self.connections.write().await;
            connections.remove(&conv)
        };

        if let Some(connection) = connection {
            let peer_addr = connection.peer_addr;
            let mut addr_to_conv = self.addr_to_conv.write().await;
            addr_to_conv.remove(&peer_addr);

            connection.close().await?;
            debug!(conv = %conv, peer = %peer_addr, "Connection removed");
        }

        Ok(())
    }

    /// Get all active connections
    pub async fn connections(&self) -> Vec<Arc<KcpConnection>> {
        let connections = self.connections.read().await;
        connections.values().cloned().collect()
    }

    /// Get connection count
    pub async fn connection_count(&self) -> usize {
        let connections = self.connections.read().await;
        connections.len()
    }

    /// Start background tasks
    async fn start_background_tasks(&mut self) -> Result<()> {
        // Update task
        let connections = self.connections.clone();
        let update_interval = self.config.nodelay.interval;

        self.update_task = Some(tokio::spawn(async move {
            let mut interval = interval(Duration::from_millis(update_interval as u64));

            loop {
                interval.tick().await;

                let connections = connections.read().await;
                for connection in connections.values() {
                    if let Err(e) = connection.update().await {
                        error!(
                            conv = %connection.conv,
                            error = %e,
                            "Connection update failed"
                        );
                    }
                }
            }
        }));

        // Receive task
        let socket = self.socket.clone();
        let connections = self.connections.clone();
        let addr_to_conv = self.addr_to_conv.clone();

        self.receive_task = Some(tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];

            loop {
                match socket.recv_from(&mut buf).await {
                    Ok((size, peer_addr)) => {
                        let data = Bytes::copy_from_slice(&buf[..size]);

                        // Find connection by peer address
                        let addr_to_conv = addr_to_conv.read().await;
                        if let Some(&conv) = addr_to_conv.get(&peer_addr) {
                            let connections = connections.read().await;
                            if let Some(connection) = connections.get(&conv) {
                                if let Err(e) = connection.input(data).await {
                                    trace!(
                                        conv = %conv,
                                        peer = %peer_addr,
                                        error = %e,
                                        "Failed to process packet"
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "UDP receive failed");
                        break;
                    }
                }
            }
        }));

        // Cleanup task
        let connections = self.connections.clone();
        let addr_to_conv = self.addr_to_conv.clone();
        let idle_timeout = self.config.keep_alive.unwrap_or(Duration::from_secs(300));

        self.cleanup_task = Some(tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(60));

            loop {
                interval.tick().await;

                let mut to_remove = Vec::new();

                // Find idle or failed connections
                {
                    let connections = connections.read().await;
                    for (&conv, connection) in connections.iter() {
                        let state = connection.state().await;
                        let is_idle = connection.is_idle(idle_timeout).await;

                        if state == ConnectionState::Failed
                            || state == ConnectionState::Closed
                            || is_idle
                        {
                            to_remove.push(conv);
                        }
                    }
                }

                // Remove connections
                for conv in to_remove {
                    let connection = {
                        let mut connections = connections.write().await;
                        connections.remove(&conv)
                    };

                    if let Some(connection) = connection {
                        let peer_addr = connection.peer_addr;
                        let mut addr_to_conv = addr_to_conv.write().await;
                        addr_to_conv.remove(&peer_addr);

                        let _ = connection.close().await;
                        debug!(conv = %conv, peer = %peer_addr, "Cleaned up connection");
                    }
                }
            }
        }));

        Ok(())
    }
}

impl Drop for ConnectionManager {
    fn drop(&mut self) {
        if let Some(task) = self.update_task.take() {
            task.abort();
        }
        if let Some(task) = self.receive_task.take() {
            task.abort();
        }
        if let Some(task) = self.cleanup_task.take() {
            task.abort();
        }
    }
}

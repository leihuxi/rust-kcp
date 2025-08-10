//! Session management and multiplexing

use crate::async_kcp::connection::{ConnectionManager, KcpConnection};
use crate::common::*;
use crate::config::KcpConfig;
use crate::error::{ConnectionError, KcpError, Result};

use bytes::Bytes;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, RwLock};
use tokio::time::{interval, Instant};
use tracing::{debug, error, info};

/// Session identifier type
pub type SessionId = u32;

/// Session represents a logical connection that can span multiple KCP connections
pub struct Session {
    id: SessionId,
    peer_addr: SocketAddr,
    connection: Arc<KcpConnection>,
    created_at: Instant,
    last_activity: Arc<tokio::sync::Mutex<Instant>>,

    // Session-specific data channels
    incoming_rx: mpsc::UnboundedReceiver<Bytes>,
    #[allow(dead_code)]
    outgoing_tx: mpsc::UnboundedSender<Bytes>,

    // Session metadata
    metadata: Arc<RwLock<HashMap<String, String>>>,
}

impl Session {
    /// Create a new session
    pub fn new(
        id: SessionId,
        peer_addr: SocketAddr,
        connection: Arc<KcpConnection>,
    ) -> (
        Self,
        mpsc::UnboundedSender<Bytes>,
        mpsc::UnboundedReceiver<Bytes>,
    ) {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, outgoing_rx) = mpsc::unbounded_channel();
        let now = Instant::now();

        let session = Self {
            id,
            peer_addr,
            connection,
            created_at: now,
            last_activity: Arc::new(tokio::sync::Mutex::new(now)),
            incoming_rx,
            outgoing_tx,
            metadata: Arc::new(RwLock::new(HashMap::new())),
        };

        (session, incoming_tx, outgoing_rx)
    }

    /// Get session ID
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// Get peer address
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    /// Send data through the session
    pub async fn send(&self, data: Bytes) -> Result<()> {
        self.connection.send(data).await?;
        self.update_activity().await;
        Ok(())
    }

    /// Receive data from the session
    pub async fn recv(&mut self) -> Result<Option<Bytes>> {
        match self.incoming_rx.try_recv() {
            Ok(data) => {
                self.update_activity().await;
                Ok(Some(data))
            }
            Err(mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(mpsc::error::TryRecvError::Disconnected) => {
                Err(KcpError::connection(ConnectionError::Closed))
            }
        }
    }

    /// Close the session
    pub async fn close(&self) -> Result<()> {
        self.connection.close().await?;
        info!(session = %self.id, peer = %self.peer_addr, "Session closed");
        Ok(())
    }

    /// Get session metadata
    pub async fn get_metadata(&self, key: &str) -> Option<String> {
        let metadata = self.metadata.read().await;
        metadata.get(key).cloned()
    }

    /// Set session metadata
    pub async fn set_metadata(&self, key: String, value: String) {
        let mut metadata = self.metadata.write().await;
        metadata.insert(key, value);
    }

    /// Get all metadata
    pub async fn metadata(&self) -> HashMap<String, String> {
        self.metadata.read().await.clone()
    }

    /// Check if session has been idle for too long
    pub async fn is_idle(&self, timeout: Duration) -> bool {
        let last_activity = *self.last_activity.lock().await;
        Instant::now().duration_since(last_activity) > timeout
    }

    /// Get session duration
    pub fn duration(&self) -> Duration {
        Instant::now().duration_since(self.created_at)
    }

    /// Update last activity timestamp
    async fn update_activity(&self) {
        let mut last_activity = self.last_activity.lock().await;
        *last_activity = Instant::now();
    }
}

/// Session statistics
#[derive(Debug, Clone)]
pub struct SessionStats {
    pub id: SessionId,
    pub peer_addr: SocketAddr,
    pub created_at: Instant,
    pub duration: Duration,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub last_activity: Instant,
}

/// Session manager for handling multiple sessions
pub struct SessionManager {
    connection_manager: ConnectionManager,

    // Session storage
    sessions: Arc<RwLock<HashMap<SessionId, Arc<Session>>>>,
    addr_to_session: Arc<RwLock<HashMap<SocketAddr, SessionId>>>,
    next_session_id: Arc<tokio::sync::Mutex<SessionId>>,

    // Configuration
    #[allow(dead_code)]
    config: KcpConfig,
    session_timeout: Duration,

    // Background tasks
    cleanup_task: Option<tokio::task::JoinHandle<()>>,
    message_router_task: Option<tokio::task::JoinHandle<()>>,

    // Event channels
    new_session_tx: mpsc::UnboundedSender<Arc<Session>>,
    #[allow(dead_code)]
    new_session_rx: mpsc::UnboundedReceiver<Arc<Session>>,
}

impl SessionManager {
    /// Create a new session manager
    pub fn new(socket: UdpSocket, config: KcpConfig) -> Self {
        let session_timeout = config.keep_alive.unwrap_or(Duration::from_secs(300));
        let connection_manager = ConnectionManager::new(socket, config.clone());
        let (new_session_tx, new_session_rx) = mpsc::unbounded_channel();

        Self {
            connection_manager,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            addr_to_session: Arc::new(RwLock::new(HashMap::new())),
            next_session_id: Arc::new(tokio::sync::Mutex::new(1)),
            config,
            session_timeout,
            cleanup_task: None,
            message_router_task: None,
            new_session_tx,
            new_session_rx,
        }
    }

    /// Start the session manager
    pub async fn start(&mut self) -> Result<()> {
        self.connection_manager.start().await?;
        self.start_background_tasks().await?;
        info!("Session manager started");
        Ok(())
    }

    /// Create a new outgoing session
    pub async fn connect(&self, peer_addr: SocketAddr) -> Result<Arc<Session>> {
        // Generate conversation and session IDs
        let conv = self.generate_conversation_id();
        let session_id = self.generate_session_id().await;

        // Create connection
        let connection = self.connection_manager.connect(peer_addr, conv).await?;

        // Create session
        let (session, incoming_tx, outgoing_rx) = Session::new(session_id, peer_addr, connection);
        let session = Arc::new(session);

        // Store session
        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(session_id, session.clone());
        }

        {
            let mut addr_to_session = self.addr_to_session.write().await;
            addr_to_session.insert(peer_addr, session_id);
        }

        // Start message routing for this session
        self.start_session_routing(session.clone(), incoming_tx, outgoing_rx)
            .await;

        self.new_session_tx
            .send(session.clone())
            .map_err(|_| KcpError::connection(ConnectionError::Closed))?;

        info!(session = %session_id, peer = %peer_addr, conv = %conv, "Session created");
        Ok(session)
    }

    /// Accept new sessions
    pub async fn accept(&mut self) -> Result<Arc<Session>> {
        // First, try to accept a new connection
        let connection = self.connection_manager.accept().await?;
        let peer_addr = connection.peer_addr;

        // Generate session ID
        let session_id = self.generate_session_id().await;

        // Create session
        let (session, incoming_tx, outgoing_rx) = Session::new(session_id, peer_addr, connection);
        let session = Arc::new(session);

        // Store session
        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(session_id, session.clone());
        }

        {
            let mut addr_to_session = self.addr_to_session.write().await;
            addr_to_session.insert(peer_addr, session_id);
        }

        // Start message routing for this session
        self.start_session_routing(session.clone(), incoming_tx, outgoing_rx)
            .await;

        info!(session = %session_id, peer = %peer_addr, "Session accepted");
        Ok(session)
    }

    /// Get session by ID
    pub async fn get_session(&self, id: SessionId) -> Option<Arc<Session>> {
        let sessions = self.sessions.read().await;
        sessions.get(&id).cloned()
    }

    /// Get session by peer address
    pub async fn get_session_by_addr(&self, addr: SocketAddr) -> Option<Arc<Session>> {
        let addr_to_session = self.addr_to_session.read().await;
        let session_id = addr_to_session.get(&addr)?;

        let sessions = self.sessions.read().await;
        sessions.get(session_id).cloned()
    }

    /// Remove a session
    pub async fn remove_session(&self, id: SessionId) -> Result<()> {
        let session = {
            let mut sessions = self.sessions.write().await;
            sessions.remove(&id)
        };

        if let Some(session) = session {
            let peer_addr = session.peer_addr();
            let mut addr_to_session = self.addr_to_session.write().await;
            addr_to_session.remove(&peer_addr);

            session.close().await?;
            debug!(session = %id, peer = %peer_addr, "Session removed");
        }

        Ok(())
    }

    /// Get all active sessions
    pub async fn sessions(&self) -> Vec<Arc<Session>> {
        let sessions = self.sessions.read().await;
        sessions.values().cloned().collect()
    }

    /// Get session count
    pub async fn session_count(&self) -> usize {
        let sessions = self.sessions.read().await;
        sessions.len()
    }

    /// Get session statistics
    pub async fn session_stats(&self) -> Vec<SessionStats> {
        let sessions = self.sessions.read().await;
        let mut stats = Vec::new();

        for session in sessions.values() {
            let connection_stats = session.connection.stats().await;
            let last_activity = *session.last_activity.lock().await;

            stats.push(SessionStats {
                id: session.id,
                peer_addr: session.peer_addr,
                created_at: session.created_at,
                duration: session.duration(),
                bytes_sent: connection_stats.bytes_sent,
                bytes_received: connection_stats.bytes_received,
                last_activity,
            });
        }

        stats
    }

    /// Generate a new conversation ID
    fn generate_conversation_id(&self) -> ConvId {
        rand::random::<u32>()
    }

    /// Generate a new session ID
    async fn generate_session_id(&self) -> SessionId {
        let mut next_id = self.next_session_id.lock().await;
        let id = *next_id;
        *next_id = next_id.wrapping_add(1);
        id
    }

    /// Start message routing for a session
    async fn start_session_routing(
        &self,
        session: Arc<Session>,
        _incoming_tx: mpsc::UnboundedSender<Bytes>,
        mut outgoing_rx: mpsc::UnboundedReceiver<Bytes>,
    ) {
        let connection = session.connection.clone();

        // Outgoing message routing
        tokio::spawn(async move {
            while let Some(data) = outgoing_rx.recv().await {
                if let Err(e) = connection.send(data).await {
                    error!(
                        session = %session.id,
                        error = %e,
                        "Failed to send data through connection"
                    );
                    break;
                }
            }
        });

        // Incoming message routing (handled by background task)
    }

    /// Start background tasks
    async fn start_background_tasks(&mut self) -> Result<()> {
        // Cleanup task
        let sessions = self.sessions.clone();
        let addr_to_session = self.addr_to_session.clone();
        let session_timeout = self.session_timeout;

        self.cleanup_task = Some(tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(60));

            loop {
                interval.tick().await;

                let mut to_remove = Vec::new();

                // Find idle sessions
                {
                    let sessions = sessions.read().await;
                    for (&id, session) in sessions.iter() {
                        if session.is_idle(session_timeout).await {
                            to_remove.push(id);
                        }
                    }
                }

                // Remove sessions
                for id in to_remove {
                    let session = {
                        let mut sessions = sessions.write().await;
                        sessions.remove(&id)
                    };

                    if let Some(session) = session {
                        let peer_addr = session.peer_addr();
                        let mut addr_to_session = addr_to_session.write().await;
                        addr_to_session.remove(&peer_addr);

                        let _ = session.close().await;
                        debug!(session = %id, peer = %peer_addr, "Cleaned up idle session");
                    }
                }
            }
        }));

        Ok(())
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        if let Some(task) = self.cleanup_task.take() {
            task.abort();
        }
        if let Some(task) = self.message_router_task.take() {
            task.abort();
        }
    }
}

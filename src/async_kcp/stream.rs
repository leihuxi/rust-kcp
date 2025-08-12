//! High-level async KCP stream interface

use crate::async_kcp::engine::{KcpEngine, OutputFn};
use crate::common::*;
use crate::config::KcpConfig;
use crate::error::{ConnectionError, KcpError, Result};

use bytes::{Buf, Bytes, BytesMut};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, Notify};
use tokio::time::{interval, MissedTickBehavior};
use tracing::{error, info, trace};

/// High-level async KCP stream providing TCP-like interface over UDP
pub struct KcpStream {
    pub(crate) engine: Arc<Mutex<KcpEngine>>,
    socket: Arc<UdpSocket>,
    peer_addr: SocketAddr,
    config: KcpConfig,

    // Read/write state
    read_buf: BytesMut,
    write_notify: Arc<Notify>,

    // Background tasks
    update_task: Option<tokio::task::JoinHandle<()>>,
    recv_task: Option<tokio::task::JoinHandle<()>>,

    // Connection state
    connected: bool,
    closed: bool,
}

impl KcpStream {
    /// Connect to a remote KCP server
    pub async fn connect(addr: SocketAddr, config: KcpConfig) -> Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0").await.map_err(KcpError::Io)?;

        socket.connect(addr).await.map_err(KcpError::Io)?;

        Self::new_with_socket(socket, addr, config, true).await
    }

    /// Create a new KCP stream with a specific conversation ID (for server side)
    pub async fn new_with_conv(
        socket: UdpSocket,
        peer_addr: SocketAddr,
        conv: ConvId,
        config: KcpConfig,
        _is_client: bool,
    ) -> Result<Self> {
        let engine = KcpEngine::new(conv, config.clone());
        let socket = Arc::new(socket);
        let engine = Arc::new(Mutex::new(engine));

        let mut stream = Self {
            engine: engine.clone(),
            socket: socket.clone(),
            peer_addr,
            config,
            read_buf: crate::common::try_get_buffer(2048),
            write_notify: Arc::new(Notify::new()),
            update_task: None,
            recv_task: None,
            connected: false,
            closed: false,
        };

        // Set up output function for the engine
        let socket_clone = socket.clone();
        let output_fn: OutputFn = Arc::new(move |data: Bytes| {
            let socket = socket_clone.clone();
            Box::pin(async move {
                let len = data.len();
                socket.send(&data).await.map_err(KcpError::Io)?;
                trace!("Client sent {} bytes via connected socket", len);
                Ok(())
            })
        });

        {
            let mut engine = stream.engine.lock().await;
            engine.set_output(output_fn);
            engine.start().await?;
        }

        // Start background tasks
        stream.start_background_tasks().await?;

        // Mark as connected
        stream.connected = true;
        info!(peer = %peer_addr, conv = conv, "KCP stream established");

        Ok(stream)
    }

    /// Process the initial packet that established the connection (for server side)
    pub async fn process_initial_packet(&mut self, data: Bytes) -> Result<()> {
        let mut engine = self.engine.lock().await;
        engine.input(data).await?;
        Ok(())
    }

    /// Process a packet from the listener (for server streams)
    pub async fn input_packet(&self, data: Bytes) -> Result<()> {
        let mut engine = self.engine.lock().await;
        engine.input(data).await?;
        // Immediately process any pending output
        engine.update().await?;
        Ok(())
    }

    /// Create a new server-side KCP stream (with proper routing)
    pub async fn new_server_stream(
        listener_socket: Arc<UdpSocket>,
        peer_addr: SocketAddr,
        conv: ConvId,
        config: KcpConfig,
        initial_packet: Bytes,
    ) -> Result<Self> {
        // For server streams, we use the shared listener socket
        // The key insight: responses must come from the same port the client connected to
        let engine = KcpEngine::new(conv, config.clone());
        let engine = Arc::new(Mutex::new(engine));

        let mut stream = Self {
            engine: engine.clone(),
            socket: listener_socket.clone(),
            peer_addr,
            config,
            read_buf: crate::common::try_get_buffer(2048),
            write_notify: Arc::new(Notify::new()),
            update_task: None,
            recv_task: None,
            connected: false,
            closed: false,
        };

        // Set up output function to send via listener socket to specific peer
        let socket_clone = listener_socket.clone();
        let peer = peer_addr;
        let output_fn: OutputFn = Arc::new(move |data: Bytes| {
            let socket = socket_clone.clone();
            Box::pin(async move {
                let len = data.len();
                socket.send_to(&data, peer).await.map_err(KcpError::Io)?;
                trace!("Sent {} bytes to {}", len, peer);
                Ok(())
            })
        });

        {
            let mut engine = stream.engine.lock().await;
            engine.set_output(output_fn);
            engine.start().await?;

            // Process the initial packet immediately
            engine.input(initial_packet).await?;

            // Force an update to send ACK/response
            engine.update().await?;
            engine.flush().await?;
        }

        // Start only update task for server stream
        // Packets are routed by the listener, not received directly
        stream.start_update_task_only().await?;

        // Mark as connected
        stream.connected = true;
        info!(peer = %peer_addr, conv = conv, "KCP server stream established (shared socket mode)");

        Ok(stream)
    }

    /// Create a new KCP stream from an existing UDP socket
    pub async fn new_with_socket(
        socket: UdpSocket,
        peer_addr: SocketAddr,
        config: KcpConfig,
        is_client: bool,
    ) -> Result<Self> {
        let conv = if is_client {
            // Use fixed conversation ID for C server compatibility
            0x12345678u32
        } else {
            // Server will use the conversation ID from the first packet
            0
        };

        Self::new_with_conv(socket, peer_addr, conv, config, is_client).await
    }

    /// Send data through the KCP stream
    pub async fn send(&mut self, data: &[u8]) -> Result<()> {
        if self.closed {
            return Err(KcpError::connection(ConnectionError::Closed));
        }

        let bytes = Bytes::copy_from_slice(data);
        let mut engine = self.engine.lock().await;
        engine.send(bytes).await?;
        // Immediately flush the data
        engine.flush().await?;

        Ok(())
    }

    /// Receive data from the KCP stream
    pub async fn recv(&mut self) -> Result<Option<Bytes>> {
        if self.closed {
            return Ok(None);
        }

        let mut engine = self.engine.lock().await;
        engine.recv().await
    }

    /// Get local address
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.socket.local_addr().map_err(KcpError::Io)
    }

    /// Get peer address
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    /// Get connection statistics
    pub async fn stats(&self) -> KcpStats {
        let engine = self.engine.lock().await;
        engine.stats().clone()
    }

    /// Check if the connection is alive
    pub async fn is_connected(&self) -> bool {
        if !self.connected || self.closed {
            return false;
        }

        let engine = self.engine.lock().await;
        !engine.is_dead()
    }

    /// Close the stream gracefully
    pub async fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }

        self.closed = true;

        // Cancel background tasks
        if let Some(task) = self.update_task.take() {
            task.abort();
        }

        if let Some(task) = self.recv_task.take() {
            task.abort();
        }

        info!(peer = %self.peer_addr, "KCP stream closed");
        Ok(())
    }

    /// Start background tasks for unconnected socket (server side)
    async fn start_background_tasks_unconnected(&mut self) -> Result<()> {
        let engine = self.engine.clone();
        let write_notify = self.write_notify.clone();
        let update_interval = self.config.nodelay.interval;

        // Update task - periodically calls engine.update()
        self.update_task = Some(tokio::spawn(async move {
            let mut interval = interval(std::time::Duration::from_millis(update_interval as u64));
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let mut engine = engine.lock().await;
                        if let Err(e) = engine.update().await {
                            error!(error = %e, "Engine update failed");
                            break;
                        }
                    }
                    _ = write_notify.notified() => {
                        // Flush immediately when notified of new data
                        let mut engine = engine.lock().await;
                        if let Err(e) = engine.flush().await {
                            error!(error = %e, "Engine flush failed");
                            break;
                        }
                    }
                }
            }
        }));

        // Receive task - receives UDP packets from any source and filters by peer_addr
        let engine = self.engine.clone();
        let socket = self.socket.clone();
        let expected_peer = self.peer_addr;

        self.recv_task = Some(tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];

            loop {
                match socket.recv_from(&mut buf).await {
                    Ok((size, src_addr)) => {
                        // Only process packets from our peer
                        if src_addr == expected_peer {
                            let data = Bytes::copy_from_slice(&buf[..size]);
                            let mut engine = engine.lock().await;
                            if let Err(e) = engine.input(data).await {
                                trace!(error = %e, "Failed to process packet");
                                // Continue processing other packets
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

        Ok(())
    }

    /// Start background tasks for update and packet receiving
    async fn start_background_tasks(&mut self) -> Result<()> {
        let engine = self.engine.clone();
        let write_notify = self.write_notify.clone();
        let update_interval = self.config.nodelay.interval;

        // Update task - periodically calls engine.update()
        self.update_task = Some(tokio::spawn(async move {
            let mut interval = interval(std::time::Duration::from_millis(update_interval as u64));
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let mut engine = engine.lock().await;
                        if let Err(e) = engine.update().await {
                            error!(error = %e, "Engine update failed");
                            break;
                        }
                    }
                    _ = write_notify.notified() => {
                        // Flush immediately when notified of new data
                        let mut engine = engine.lock().await;
                        if let Err(e) = engine.flush().await {
                            error!(error = %e, "Engine flush failed");
                            break;
                        }
                    }
                }
            }
        }));

        // Receive task - receives UDP packets and feeds them to engine
        let engine = self.engine.clone();
        let socket = self.socket.clone();

        self.recv_task = Some(tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];

            loop {
                match socket.recv(&mut buf).await {
                    Ok(size) => {
                        let data = Bytes::copy_from_slice(&buf[..size]);
                        trace!("Client received {} bytes", size);
                        let mut engine = engine.lock().await;

                        if let Err(e) = engine.input(data).await {
                            trace!(error = %e, "Failed to process packet");
                            // Continue processing other packets
                        } else {
                            trace!("Successfully processed {} bytes", size);
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "UDP receive failed");
                        break;
                    }
                }
            }
        }));

        Ok(())
    }

    /// Start only the update task (for server streams)
    async fn start_update_task_only(&mut self) -> Result<()> {
        let engine = self.engine.clone();
        let write_notify = self.write_notify.clone();
        let update_interval = self.config.nodelay.interval;

        // Update task - periodically calls engine.update()
        self.update_task = Some(tokio::spawn(async move {
            let mut interval = interval(std::time::Duration::from_millis(update_interval as u64));
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let mut engine = engine.lock().await;
                        if let Err(e) = engine.update().await {
                            error!(error = %e, "Engine update failed");
                            break;
                        }
                    }
                    _ = write_notify.notified() => {
                        // Flush immediately when notified of new data
                        let mut engine = engine.lock().await;
                        if let Err(e) = engine.flush().await {
                            error!(error = %e, "Engine flush failed");
                            break;
                        }
                    }
                }
            }
        }));

        Ok(())
    }
}

impl Drop for KcpStream {
    fn drop(&mut self) {
        // Cancel background tasks
        if let Some(task) = self.update_task.take() {
            task.abort();
        }

        if let Some(task) = self.recv_task.take() {
            task.abort();
        }

        // Return read buffer to pool for memory efficiency
        if !self.read_buf.is_empty() {
            let mut buf = std::mem::replace(&mut self.read_buf, BytesMut::new());
            buf.clear();
            crate::common::try_put_buffer(buf);
        }
    }
}

// Implement AsyncRead trait for KcpStream
impl AsyncRead for KcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.closed {
            return Poll::Ready(Ok(()));
        }

        // Check if we have data in our buffer
        if !self.read_buf.is_empty() {
            let to_copy = std::cmp::min(buf.remaining(), self.read_buf.len());
            buf.put_slice(&self.read_buf[..to_copy]);
            self.read_buf.advance(to_copy);
            return Poll::Ready(Ok(()));
        }

        // Try to receive data from KCP
        let engine = self.engine.clone();
        let mut recv_future = Box::pin(async move {
            let mut engine = engine.lock().await;
            engine.recv().await
        });

        match recv_future.as_mut().poll(cx) {
            Poll::Ready(Ok(Some(data))) => {
                self.read_buf.extend_from_slice(&data);
                let to_copy = std::cmp::min(buf.remaining(), self.read_buf.len());
                buf.put_slice(&self.read_buf[..to_copy]);
                self.read_buf.advance(to_copy);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Ok(None)) => {
                // No data available, would block
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(io::Error::other(e))),
            Poll::Pending => Poll::Pending,
        }
    }
}

// Implement AsyncWrite trait for KcpStream
impl AsyncWrite for KcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.closed {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Stream is closed",
            )));
        }

        let engine = self.engine.clone();
        let data = Bytes::copy_from_slice(buf);
        let write_notify = self.write_notify.clone();

        let mut send_future = Box::pin(async move {
            let mut engine = engine.lock().await;
            engine.send(data).await
        });

        match send_future.as_mut().poll(cx) {
            Poll::Ready(Ok(())) => {
                write_notify.notify_one();
                Poll::Ready(Ok(buf.len()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(io::Error::other(e))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let engine = self.engine.clone();
        let mut flush_future = Box::pin(async move {
            let mut engine = engine.lock().await;
            // Ensure data is sent immediately
            engine.flush().await?;
            engine.update().await
        });

        match flush_future.as_mut().poll(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(io::Error::other(e))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.closed = true;
        Poll::Ready(Ok(()))
    }
}

// Make KcpStream safe to send between threads
unsafe impl Send for KcpStream {}
unsafe impl Sync for KcpStream {}

//! High-level async KCP stream interface

use crate::async_kcp::actor::{run_engine_actor, EngineHandle};
use crate::async_kcp::engine::KcpEngine;
use crate::common::*;
use crate::config::KcpConfig;
use crate::error::{ConnectionError, KcpError, Result};
use crate::transport::Transport;

use bytes::{Buf, Bytes, BytesMut};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tracing::{info, trace};

/// High-level async KCP stream providing TCP-like interface over UDP
pub struct KcpStream {
    handle: EngineHandle,
    pub(crate) input_tx: mpsc::Sender<Bytes>,
    data_rx: mpsc::Receiver<Bytes>,
    transport: Arc<dyn Transport>,
    peer_addr: SocketAddr,

    // Read/write state
    read_buf: BytesMut,
    pending_write: Option<Pin<Box<dyn Future<Output = io::Result<usize>> + Send>>>,
    pending_flush: Option<Pin<Box<dyn Future<Output = io::Result<()>> + Send>>>,

    // Background tasks
    actor_task: Option<tokio::task::JoinHandle<()>>,
    recv_task: Option<tokio::task::JoinHandle<()>>,

    // Connection state
    connected: bool,
    closed: bool,
}

impl KcpStream {
    /// Connect to a remote KCP server
    pub async fn connect(addr: SocketAddr, config: KcpConfig) -> Result<Self> {
        let transport = crate::transport::UdpTransport::bind("0.0.0.0:0")
            .await
            .map_err(KcpError::Io)?;
        let transport = Arc::new(transport);

        let local_addr = transport.local_addr().map_err(KcpError::Io)?;
        trace!("CLIENT: Bound to local address {}", local_addr);
        trace!("CLIENT: Targeting remote address {}", addr);

        Self::connect_with_transport(transport, addr, config).await
    }

    /// Connect to a remote KCP server using a custom [`Transport`].
    pub async fn connect_with_transport(
        transport: Arc<dyn Transport>,
        addr: SocketAddr,
        config: KcpConfig,
    ) -> Result<Self> {
        Self::new_with_transport(transport, addr, config, true).await
    }

    /// Create a new KCP stream with a specific conversation ID (for server side)
    pub async fn new_with_conv(
        transport: Arc<dyn Transport>,
        peer_addr: SocketAddr,
        conv: ConvId,
        config: KcpConfig,
    ) -> Result<Self> {
        let mut engine = KcpEngine::new(conv, config.clone());
        engine.start()?;

        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (input_tx, input_rx) = mpsc::channel(256);
        let (data_tx, data_rx) = mpsc::channel(256);
        let handle = EngineHandle::new(cmd_tx);

        let update_interval = config.nodelay.interval as u64;
        let keep_alive_ms = config.keep_alive.map(|d| d.as_millis() as u64);

        let transport_actor = transport.clone();
        let actor_task = tokio::spawn(run_engine_actor(
            engine,
            cmd_rx,
            input_rx,
            data_tx,
            transport_actor,
            peer_addr,
            update_interval,
            keep_alive_ms,
        ));

        // Client: spawn recv_task that reads from transport → input_tx
        let recv_task = Self::spawn_recv_task(transport.clone(), input_tx.clone(), peer_addr);

        let stream = Self {
            handle,
            input_tx,
            data_rx,
            transport,
            peer_addr,
            read_buf: crate::common::try_get_buffer(2048),
            pending_write: None,
            pending_flush: None,
            actor_task: Some(actor_task),
            recv_task: Some(recv_task),
            connected: true,
            closed: false,
        };

        info!(peer = %peer_addr, conv = conv, "KCP stream established");
        Ok(stream)
    }

    /// Create a new server-side KCP stream (with proper routing)
    pub async fn new_server_stream(
        transport: Arc<dyn Transport>,
        peer_addr: SocketAddr,
        conv: ConvId,
        config: KcpConfig,
        initial_packet: Bytes,
    ) -> Result<Self> {
        let mut engine = KcpEngine::new(conv, config.clone());
        engine.start()?;

        // Process the initial packet synchronously
        engine.input(initial_packet)?;
        engine.update()?;
        engine.flush()?;

        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (input_tx, input_rx) = mpsc::channel(256);
        let (data_tx, data_rx) = mpsc::channel(256);
        let handle = EngineHandle::new(cmd_tx);

        let update_interval = config.nodelay.interval as u64;
        let keep_alive_ms = config.keep_alive.map(|d| d.as_millis() as u64);

        let transport_actor = transport.clone();
        let actor_task = tokio::spawn(run_engine_actor(
            engine,
            cmd_rx,
            input_rx,
            data_tx,
            transport_actor,
            peer_addr,
            update_interval,
            keep_alive_ms,
        ));

        // Server streams don't have a recv_task — packets are routed by the listener
        let stream = Self {
            handle,
            input_tx,
            data_rx,
            transport,
            peer_addr,
            read_buf: crate::common::try_get_buffer(2048),
            pending_write: None,
            pending_flush: None,
            actor_task: Some(actor_task),
            recv_task: None,
            connected: true,
            closed: false,
        };

        info!(peer = %peer_addr, conv = conv, "KCP server stream established");
        Ok(stream)
    }

    /// Create a new KCP stream from an existing transport
    pub async fn new_with_transport(
        transport: Arc<dyn Transport>,
        peer_addr: SocketAddr,
        config: KcpConfig,
        is_client: bool,
    ) -> Result<Self> {
        let conv = if is_client {
            0x12345678u32 // Fixed conversation ID for C server compatibility
        } else {
            0 // Server will use the conversation ID from the first packet
        };
        Self::new_with_conv(transport, peer_addr, conv, config).await
    }

    /// Get a clone of the input sender channel (for listener packet routing)
    pub(crate) fn input_sender(&self) -> mpsc::Sender<Bytes> {
        self.input_tx.clone()
    }

    /// Send data through the KCP stream
    pub async fn send(&self, data: &[u8]) -> Result<()> {
        if self.closed {
            return Err(KcpError::connection(ConnectionError::Closed));
        }
        self.handle.send(Bytes::copy_from_slice(data)).await
    }

    /// Receive data from the KCP stream
    pub async fn recv(&mut self) -> Result<Option<Bytes>> {
        if self.closed {
            return Ok(None);
        }
        Ok(self.data_rx.recv().await)
    }

    /// Get local address
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.transport.local_addr().map_err(KcpError::Io)
    }

    /// Get peer address
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    /// Get connection statistics
    pub async fn stats(&self) -> KcpStats {
        self.handle.stats().await.unwrap_or_default()
    }

    /// Check if the connection is alive
    pub async fn is_connected(&self) -> bool {
        self.connected && !self.closed && self.handle.is_alive().await
    }

    /// Close the stream gracefully
    pub async fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;

        // Signal actor to flush and stop
        self.handle.close();

        // Wait briefly for the actor to finish flushing
        if let Some(task) = self.actor_task.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(1), task).await;
        }

        self.abort_tasks();
        info!(peer = %self.peer_addr, "KCP stream closed");
        Ok(())
    }

    /// Abort background tasks
    fn abort_tasks(&mut self) {
        if let Some(task) = self.actor_task.take() {
            task.abort();
        }
        if let Some(task) = self.recv_task.take() {
            task.abort();
        }
    }

    /// Spawn a recv_task that reads from transport and feeds input_tx
    fn spawn_recv_task(
        transport: Arc<dyn Transport>,
        input_tx: mpsc::Sender<Bytes>,
        peer_addr: SocketAddr,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            loop {
                match transport.recv_from(&mut buf).await {
                    Ok((size, src_addr)) => {
                        if src_addr != peer_addr {
                            trace!(
                                src = %src_addr,
                                expected = %peer_addr,
                                "Ignoring packet from unexpected source"
                            );
                            continue;
                        }
                        let data = Bytes::copy_from_slice(&buf[..size]);
                        if input_tx.send(data).await.is_err() {
                            trace!("Input channel closed, stopping recv task");
                            return;
                        }
                    }
                    Err(e) => {
                        trace!(error = %e, "Transport receive failed, stopping recv task");
                        break;
                    }
                }
            }
        })
    }
}

impl Drop for KcpStream {
    fn drop(&mut self) {
        // Signal actor to shut down
        self.handle.close();
        self.abort_tasks();

        // Return read buffer to pool for memory efficiency
        if !self.read_buf.is_empty() {
            let mut buf = std::mem::take(&mut self.read_buf);
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
        trace!("poll_read called");

        if self.closed {
            return Poll::Ready(Ok(()));
        }

        // Check if we have data in our buffer first
        if !self.read_buf.is_empty() {
            let to_copy = std::cmp::min(buf.remaining(), self.read_buf.len());
            buf.put_slice(&self.read_buf[..to_copy]);
            self.read_buf.advance(to_copy);
            trace!("Returning {} bytes from buffer", to_copy);
            return Poll::Ready(Ok(()));
        }

        // Poll the channel receiver for data from actor
        match Pin::new(&mut self.data_rx).poll_recv(cx) {
            Poll::Ready(Some(data)) => {
                self.read_buf.extend_from_slice(&data);
                let to_copy = std::cmp::min(buf.remaining(), self.read_buf.len());
                buf.put_slice(&self.read_buf[..to_copy]);
                self.read_buf.advance(to_copy);
                trace!("Received {} bytes from channel", to_copy);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(None) => {
                trace!("Data channel closed");
                Poll::Ready(Ok(())) // EOF
            }
            Poll::Pending => {
                trace!("No data available, waiting");
                Poll::Pending
            }
        }
    }
}

// Implement AsyncWrite trait for KcpStream
impl AsyncWrite for KcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.closed {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Stream is closed",
            )));
        }

        // Reuse pending future if it exists (don't recreate on every poll)
        if self.pending_write.is_none() {
            let handle = self.handle.clone();
            let data = Bytes::copy_from_slice(buf);
            let len = buf.len();
            self.pending_write = Some(Box::pin(async move {
                handle.send(data).await.map_err(io::Error::other)?;
                Ok(len)
            }));
        }

        match self.pending_write.as_mut().unwrap().as_mut().poll(cx) {
            Poll::Ready(result) => {
                self.pending_write = None;
                Poll::Ready(result)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.closed {
            return Poll::Ready(Ok(()));
        }

        // Reuse pending flush future if it exists (don't recreate on every poll)
        if self.pending_flush.is_none() {
            let handle = self.handle.clone();
            self.pending_flush = Some(Box::pin(async move {
                handle.flush().await.map_err(io::Error::other)
            }));
        }

        match self.pending_flush.as_mut().unwrap().as_mut().poll(cx) {
            Poll::Ready(result) => {
                self.pending_flush = None;
                Poll::Ready(result)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.closed = true;
        self.handle.close();
        Poll::Ready(Ok(()))
    }
}

//! High-level async KCP stream interface

use crate::actor::{run_engine_actor, EngineHandle};
use crate::engine::KcpEngine;
use crate::common::*;
use crate::config::KcpConfig;
use crate::error::{ConnectionError, KcpError, Result};
use crate::transport::{Transport, UdpTransport};

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

/// Capacity of the bounded outbound-data channel between the stream and its
/// actor. Small on purpose — the real send buffer lives in the engine, bounded
/// by [`send_high_water`]; this just hands data across without itself becoming
/// an unbounded queue.
const SEND_CHANNEL_CAPACITY: usize = 16;

/// High-water mark (in queued messages) at which the actor stops accepting new
/// writes, applying backpressure. Scaled off the send window so a writer can
/// stay a few windows ahead of the network without growing the queue forever.
fn send_high_water(config: &KcpConfig) -> usize {
    (config.snd_wnd as usize * 4).max(64)
}

/// High-level async KCP stream providing TCP-like interface over UDP
pub struct KcpStream<T: Transport = UdpTransport> {
    handle: EngineHandle,
    /// Bounded channel carrying outbound application data to the actor. Its
    /// capacity is the write backpressure: a full channel blocks `send` /
    /// `poll_write` instead of growing the engine's send queue without bound.
    send_tx: mpsc::Sender<Bytes>,
    pub(crate) input_tx: mpsc::Sender<Bytes>,
    data_rx: mpsc::Receiver<Bytes>,
    transport: Arc<T>,
    peer_addr: T::Addr,
    /// Max segment size (mtu − header). `poll_write` chunks the byte stream to
    /// this so no single KCP message exceeds the engine's fragment limit.
    mss: usize,
    /// Largest single message [`send`](Self::send) accepts: `mss` × the
    /// fragment count the engine can reassemble (bounded by `rcv_wnd`,
    /// assuming symmetric peer configuration).
    max_msg: usize,

    // Read/write state
    read_buf: BytesMut,
    pending_write: Option<Pin<Box<dyn Future<Output = io::Result<usize>> + Send>>>,
    pending_flush: Option<Pin<Box<dyn Future<Output = io::Result<()>> + Send>>>,
    pending_shutdown: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,

    // Background tasks
    actor_task: Option<tokio::task::JoinHandle<()>>,
    recv_task: Option<tokio::task::JoinHandle<()>>,

    // Connection state
    connected: bool,
    closed: bool,
}

// --- UDP-specific convenience methods ---

impl KcpStream<UdpTransport> {
    /// Connect to a remote KCP server
    pub async fn connect(addr: SocketAddr, config: KcpConfig) -> Result<Self> {
        // Bind in the target's address family — a v4 wildcard socket cannot
        // send to a v6 peer (every send_to would fail, surfacing as a stream
        // that connects but never receives).
        let bind_addr = if addr.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
        let transport = UdpTransport::bind(bind_addr)
            .await
            .map_err(KcpError::Io)?;
        let transport = Arc::new(transport);

        let local_addr = transport.local_addr().map_err(KcpError::Io)?;
        trace!("CLIENT: Bound to local address {}", local_addr);
        trace!("CLIENT: Targeting remote address {}", addr);

        Self::connect_with_transport(transport, addr, config).await
    }
}

// --- Generic methods for any Transport ---

impl<T: Transport> KcpStream<T> {
    /// Connect to a remote KCP server using a custom [`Transport`].
    pub async fn connect_with_transport(
        transport: Arc<T>,
        addr: T::Addr,
        config: KcpConfig,
    ) -> Result<Self> {
        Self::new_with_transport(transport, addr, config, true).await
    }

    /// Create a new KCP stream with a specific conversation ID (for server side)
    pub async fn new_with_conv(
        transport: Arc<T>,
        peer_addr: T::Addr,
        conv: ConvId,
        config: KcpConfig,
    ) -> Result<Self> {
        config.validate()?;
        let mut engine = KcpEngine::new(conv, config.clone());
        engine.start()?;

        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (send_tx, send_rx) = mpsc::channel(SEND_CHANNEL_CAPACITY);
        let (input_tx, input_rx) = mpsc::channel(256);
        let (data_tx, data_rx) = mpsc::channel(256);
        let handle = EngineHandle::new(cmd_tx);

        let update_interval = config.nodelay.interval as u64;
        let keep_alive_ms = config.keep_alive.map(|d| d.as_millis() as u64);
        let simulate_loss = config.simulate_packet_loss;
        let send_high_water = send_high_water(&config);
        let mss = config.mtu.saturating_sub(constants::IKCP_OVERHEAD).max(1) as usize;
        let max_msg = mss * config.rcv_wnd.min(constants::IKCP_WND_RCV) as usize;

        let transport_actor = transport.clone();
        let actor_peer = peer_addr.clone();
        let actor_task = tokio::spawn(run_engine_actor(
            engine,
            cmd_rx,
            send_rx,
            input_rx,
            data_tx,
            transport_actor,
            actor_peer,
            update_interval,
            keep_alive_ms,
            simulate_loss,
            send_high_water,
        ));

        // Client: spawn recv_task that reads from transport → input_tx
        let recv_task = Self::spawn_recv_task(transport.clone(), input_tx.clone(), peer_addr.clone());

        let stream = Self {
            handle,
            send_tx,
            input_tx,
            data_rx,
            transport,
            peer_addr,
            mss,
            max_msg,
            read_buf: BytesMut::with_capacity(2048),
            pending_write: None,
            pending_flush: None,
            pending_shutdown: None,
            actor_task: Some(actor_task),
            recv_task: Some(recv_task),
            connected: true,
            closed: false,
        };

        info!(peer = %stream.peer_addr, conv = conv, "KCP stream established");
        Ok(stream)
    }

    /// Create a new server-side KCP stream (with proper routing).
    ///
    /// `client_conv` is the `conv` seen on the wire in the first packet; the
    /// engine is constructed with it so `input()` can parse that packet.
    /// `server_conv` is the conv the server will use for outgoing segments —
    /// when the two differ (client sent `conv=0`), this performs the tokio_kcp
    /// handshake by switching the engine's conv before the first flush.
    pub async fn new_server_stream(
        transport: Arc<T>,
        peer_addr: T::Addr,
        client_conv: ConvId,
        server_conv: ConvId,
        config: KcpConfig,
        initial_packet: Bytes,
    ) -> Result<Self> {
        config.validate()?;
        let mut engine = KcpEngine::new(client_conv, config.clone());
        engine.start()?;

        // Process the initial packet under the client's conv (may be 0), then
        // switch to the server-assigned non-zero conv for all outgoing traffic.
        engine.input(initial_packet)?;
        if client_conv != server_conv {
            engine.set_conv(server_conv);
        }
        engine.update()?;
        engine.flush()?;

        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (send_tx, send_rx) = mpsc::channel(SEND_CHANNEL_CAPACITY);
        let (input_tx, input_rx) = mpsc::channel(256);
        let (data_tx, data_rx) = mpsc::channel(256);
        let handle = EngineHandle::new(cmd_tx);

        let update_interval = config.nodelay.interval as u64;
        let keep_alive_ms = config.keep_alive.map(|d| d.as_millis() as u64);
        let simulate_loss = config.simulate_packet_loss;
        let send_high_water = send_high_water(&config);
        let mss = config.mtu.saturating_sub(constants::IKCP_OVERHEAD).max(1) as usize;
        let max_msg = mss * config.rcv_wnd.min(constants::IKCP_WND_RCV) as usize;

        let transport_actor = transport.clone();
        let actor_peer = peer_addr.clone();
        let actor_task = tokio::spawn(run_engine_actor(
            engine,
            cmd_rx,
            send_rx,
            input_rx,
            data_tx,
            transport_actor,
            actor_peer,
            update_interval,
            keep_alive_ms,
            simulate_loss,
            send_high_water,
        ));

        // Server streams don't have a recv_task — packets are routed by the listener
        let stream = Self {
            handle,
            send_tx,
            input_tx,
            data_rx,
            transport,
            peer_addr,
            mss,
            max_msg,
            read_buf: BytesMut::with_capacity(2048),
            pending_write: None,
            pending_flush: None,
            pending_shutdown: None,
            actor_task: Some(actor_task),
            recv_task: None,
            connected: true,
            closed: false,
        };

        info!(
            peer = %stream.peer_addr,
            client_conv,
            server_conv,
            "KCP server stream established"
        );
        Ok(stream)
    }

    /// Create a new KCP stream from an existing transport
    pub async fn new_with_transport(
        transport: Arc<T>,
        peer_addr: T::Addr,
        config: KcpConfig,
        is_client: bool,
    ) -> Result<Self> {
        let conv = if is_client {
            random_conv_id() // Random conv ID per connection to avoid collisions
        } else {
            0 // Server will use the conversation ID from the first packet
        };
        Self::new_with_conv(transport, peer_addr, conv, config).await
    }

    /// Get a clone of the input sender channel (for listener packet routing)
    pub(crate) fn input_sender(&self) -> mpsc::Sender<Bytes> {
        self.input_tx.clone()
    }

    /// Send data through the KCP stream.
    ///
    /// Awaits if the bounded send channel is full (backpressure) and returns a
    /// closed error if the actor has exited.
    pub async fn send(&self, data: &[u8]) -> Result<()> {
        if self.closed {
            return Err(KcpError::connection(ConnectionError::Closed));
        }
        // The engine rejects a single message with more fragments than the
        // receive window can reassemble; fail fast with a clear error rather
        // than letting it be silently dropped in the actor.
        if data.len() > self.max_msg {
            return Err(KcpError::buffer(format!(
                "message of {} bytes exceeds max {} for one send; use AsyncWrite for streaming",
                data.len(),
                self.max_msg
            )));
        }
        self.send_tx
            .send(Bytes::copy_from_slice(data))
            .await
            .map_err(|_| KcpError::connection(ConnectionError::Closed))
    }

    /// Receive data from the KCP stream
    pub async fn recv(&mut self) -> Result<Option<Bytes>> {
        if self.closed {
            return Ok(None);
        }
        Ok(self.data_rx.recv().await)
    }

    /// Get local address
    pub fn local_addr(&self) -> Result<T::Addr> {
        self.transport.local_addr().map_err(KcpError::Io)
    }

    /// Get peer address
    pub fn peer_addr(&self) -> &T::Addr {
        &self.peer_addr
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

        // Signal actor to flush and stop. Awaits channel capacity so the
        // Close command can't be dropped when the actor is momentarily busy.
        self.handle.close_graceful().await;

        // Wait for the actor to finish its graceful drain (it breaks as soon as
        // the peer has acknowledged everything; this is just an upper bound).
        if let Some(task) = self.actor_task.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), task).await;
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
        transport: Arc<T>,
        input_tx: mpsc::Sender<Bytes>,
        peer_addr: T::Addr,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            loop {
                let recv = tokio::select! {
                    r = transport.recv_from(&mut buf) => r,
                    // Actor exited (graceful close finished). Without this the
                    // task — and the socket it holds — would park in recv_from
                    // forever on a quiet link, since Drop no longer aborts it.
                    _ = input_tx.closed() => {
                        trace!("Input channel closed, stopping recv task");
                        return;
                    }
                };
                match recv {
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

// KcpStream contains no self-referential pinned data, so it is safe to Unpin.
impl<T: Transport> Unpin for KcpStream<T> {}

impl<T: Transport> Drop for KcpStream<T> {
    fn drop(&mut self) {
        // Best-effort Close signal; even if the cmd channel is full, dropping
        // our cmd_tx below closes the channel, which the actor also treats as
        // Close. The actor is deliberately NOT aborted: it keeps
        // retransmitting until the peer has acknowledged all in-flight data
        // (or its linger deadline passes), so dropping a stream right after
        // writing doesn't lose the tail. The recv_task exits on its own once
        // the actor is gone (see `input_tx.closed()` in spawn_recv_task).
        self.handle.close();
    }
}

// Implement AsyncRead trait for KcpStream
impl<T: Transport> AsyncRead for KcpStream<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        trace!("poll_read called");

        // Drain locally buffered data even when closed — `closed` only means
        // no more data will arrive, not that already-received bytes vanish.
        if !self.read_buf.is_empty() {
            let to_copy = std::cmp::min(buf.remaining(), self.read_buf.len());
            buf.put_slice(&self.read_buf[..to_copy]);
            self.read_buf.advance(to_copy);
            trace!("Returning {} bytes from buffer", to_copy);
            return Poll::Ready(Ok(()));
        }

        if self.closed {
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
impl<T: Transport> AsyncWrite for KcpStream<T> {
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

        // The write is chunked to one MSS so each KCP message is a single
        // segment — the byte-stream has no message boundaries to preserve, and
        // this keeps any one message well under the engine's fragment limit.
        if self.pending_write.is_none() {
            let len = buf.len().min(self.mss);
            let data = Bytes::copy_from_slice(&buf[..len]);
            // Fast path: hand the chunk off without allocating a future (this
            // boxed future per MSS chunk was the hot-path allocation in
            // poll_write). Only a full channel — real backpressure — pays for
            // the future that parks the writer on channel capacity.
            match self.send_tx.try_send(data) {
                Ok(()) => return Poll::Ready(Ok(len)),
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "connection closed",
                    )));
                }
                Err(mpsc::error::TrySendError::Full(data)) => {
                    let send_tx = self.send_tx.clone();
                    self.pending_write = Some(Box::pin(async move {
                        send_tx.send(data).await.map_err(io::Error::other)?;
                        Ok(len)
                    }));
                }
            }
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

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.closed {
            return Poll::Ready(Ok(()));
        }

        // Graceful shutdown: deliver Close to the actor, then wait for it to
        // finish draining (peer ACKs all in-flight data, or the actor's linger
        // deadline passes). Returning Ready before the drain completes would
        // let callers drop the stream and lose the unacknowledged tail.
        if self.pending_shutdown.is_none() {
            let handle = self.handle.clone();
            let actor = self.actor_task.take();
            self.pending_shutdown = Some(Box::pin(async move {
                handle.close_graceful().await;
                if let Some(actor) = actor {
                    let _ = actor.await;
                }
            }));
        }

        match self.pending_shutdown.as_mut().unwrap().as_mut().poll(cx) {
            Poll::Ready(()) => {
                self.pending_shutdown = None;
                self.closed = true;
                Poll::Ready(Ok(()))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

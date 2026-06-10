//! Actor-based engine driver — owns the KcpEngine in a dedicated task,
//! communicates via channels. Zero locks on the hot path.

use crate::engine::KcpEngine;
use crate::common::KcpStats;
use crate::error::{KcpError, Result};
use crate::transport::Transport;

use bytes::Bytes;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, trace, warn};

/// Control commands sent to the engine actor.
///
/// Application *data* does not travel here — it goes through a separate bounded
/// `Bytes` channel (see [`run_engine_actor`]) so writes get real backpressure
/// instead of unbounded queuing. Only low-frequency control ops use this path.
pub(crate) enum EngineCmd {
    Flush {
        reply: oneshot::Sender<Result<()>>,
    },
    Stats {
        reply: oneshot::Sender<KcpStats>,
    },
    IsAlive {
        reply: oneshot::Sender<bool>,
    },
    Close,
}

/// Clonable, lock-free handle to the engine actor.
#[derive(Clone)]
pub(crate) struct EngineHandle {
    cmd_tx: mpsc::Sender<EngineCmd>,
}

impl EngineHandle {
    pub fn new(cmd_tx: mpsc::Sender<EngineCmd>) -> Self {
        Self { cmd_tx }
    }

    /// Send a command and wait for the reply. Returns a connection-closed error
    /// if the actor has exited.
    async fn request<T>(
        &self,
        cmd: impl FnOnce(oneshot::Sender<T>) -> EngineCmd,
    ) -> Result<T> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(cmd(reply))
            .await
            .map_err(|_| crate::error::KcpError::connection(crate::error::ConnectionError::Closed))?;
        rx.await
            .map_err(|_| crate::error::KcpError::connection(crate::error::ConnectionError::Closed))
    }

    pub async fn flush(&self) -> Result<()> {
        self.request(|reply| EngineCmd::Flush { reply }).await?
    }

    pub async fn stats(&self) -> Result<KcpStats> {
        self.request(|reply| EngineCmd::Stats { reply }).await
    }

    pub async fn is_alive(&self) -> bool {
        self.request(|reply| EngineCmd::IsAlive { reply })
            .await
            .unwrap_or(false)
    }

    pub fn close(&self) {
        let _ = self.cmd_tx.try_send(EngineCmd::Close);
    }

    /// Deliver `Close` reliably: unlike [`close`](Self::close), waits for
    /// channel capacity instead of silently dropping the command when the
    /// actor is busy. Used by graceful shutdown, where a lost Close would
    /// leave the caller waiting on an actor that never starts draining.
    pub async fn close_graceful(&self) {
        let _ = self.cmd_tx.send(EngineCmd::Close).await;
    }
}

/// Run the engine actor loop.
///
/// - `input_rx`: raw UDP packets from recv_task (client) or listener (server).
/// - `data_tx`: assembled application messages forwarded to user reads.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_engine_actor<T: Transport>(
    mut engine: KcpEngine,
    mut cmd_rx: mpsc::Receiver<EngineCmd>,
    mut send_rx: mpsc::Receiver<Bytes>,
    mut input_rx: mpsc::Receiver<Bytes>,
    data_tx: mpsc::Sender<Bytes>,
    transport: Arc<T>,
    peer_addr: T::Addr,
    update_interval_ms: u64,
    keep_alive_ms: Option<u64>,
    simulate_loss: Option<f32>,
    send_high_water: usize,
) {
    // Event-driven wakeups: each iteration sleeps until the engine's next real
    // deadline (`engine.check()`) instead of a fixed tick, capped so keep-alive
    // still fires and a connection never sleeps unboundedly. `update_interval_ms`
    // remains the floor so we never busy-spin on a 0 deadline.
    let idle_cap_ms = keep_alive_ms.unwrap_or(1000).max(update_interval_ms.max(1));
    // Throttles keep-alive probes to one per keep-alive window (idle_ms() does
    // not reset on a probe, so gating on it alone would re-probe every tick).
    let mut last_probe = tokio::time::Instant::now();
    // Set once every write handle has dropped, to disable the send branch (which
    // would otherwise busy-loop on `recv()` returning `None`).
    let mut send_closed = false;
    // Graceful close: when set, stop accepting new writes/commands and keep
    // flushing + retransmitting until the peer has acknowledged all in-flight
    // data (or the linger deadline passes), so the tail of a stream isn't lost.
    let mut closing = false;
    let mut close_deadline: Option<tokio::time::Instant> = None;

    // Initial update + flush + drain any pre-loaded messages
    // (server streams may have processed initial packets before spawning the actor)
    let _ = engine.update();
    flush_output(&mut engine, &transport, &peer_addr, simulate_loss).await;
    drain_recv(&mut engine, &data_tx);

    loop {
        // Sleep until the engine's next real deadline (retransmit / probe / ACK)
        // rather than a fixed interval. Floored at 1ms so a `0` deadline can't
        // busy-spin, capped at `idle_cap_ms` so keep-alive still fires.
        let wait = Duration::from_millis((engine.check() as u64).clamp(1, idle_cap_ms));
        let timer = tokio::time::sleep(wait);

        tokio::select! {
            biased;

            // Deadline reached: flush (retransmits, delayed ACKs, probes). Uses
            // flush() not update() — update() re-gates on the fixed interval,
            // which would skip a retransmit we deliberately woke early for.
            _ = timer => {
                if let Err(e) = engine.flush() {
                    if e.is_fatal() {
                        error!(error = %e, "Engine flush (timer) fatal error, stopping actor");
                        break;
                    }
                    warn!(error = %e, "Engine flush (timer) failed (recoverable)");
                }

                // Keep-alive: probe at most once per keep-alive window.
                if let Some(ka) = keep_alive_ms {
                    // Dead-peer detection: `idle_ms` resets on any received
                    // packet, and a live peer answers WASK probes with WINS,
                    // so three silent keep-alive windows mean the peer is
                    // gone. Without this, a silently-vanished peer leaves the
                    // actor (and its server-side stream) alive forever —
                    // probes aren't retransmitted and never count toward
                    // `dead_link`.
                    if engine.idle_ms() as u64 >= ka.saturating_mul(3) {
                        warn!(
                            idle_ms = engine.idle_ms(),
                            keep_alive_ms = ka,
                            "Peer silent for 3 keep-alive windows, closing connection"
                        );
                        break;
                    }
                    if engine.idle_ms() as u64 >= ka
                        && last_probe.elapsed() >= std::time::Duration::from_millis(ka)
                    {
                        last_probe = tokio::time::Instant::now();
                        if let Err(e) = engine.keep_alive_probe() {
                            if e.is_fatal() {
                                error!(error = %e, "Keep-alive probe fatal");
                                break;
                            }
                            warn!(error = %e, "Keep-alive probe failed");
                        }
                    }
                }

                flush_output(&mut engine, &transport, &peer_addr, simulate_loss).await;
                // Forward any messages that backpressure left buffered in the
                // engine on a previous tick/input now that data_rx may have room.
                drain_recv(&mut engine, &data_tx);
            }

            // User commands (disabled once closing — we're draining, not serving)
            cmd = cmd_rx.recv(), if !closing => {
                match cmd {
                    Some(EngineCmd::Flush { reply }) => {
                        let r = engine.flush().map_err(KcpError::from);
                        flush_output(&mut engine, &transport, &peer_addr, simulate_loss).await;
                        let _ = reply.send(r);
                    }
                    Some(EngineCmd::Stats { reply }) => {
                        let _ = reply.send(*engine.stats());
                    }
                    Some(EngineCmd::IsAlive { reply }) => {
                        let _ = reply.send(!engine.is_dead());
                    }
                    Some(EngineCmd::Close) | None => {
                        // Begin graceful shutdown: absorb any still-buffered
                        // writes, stop accepting more, and enter the drain loop
                        // (the post-select check below breaks once everything is
                        // acknowledged or the linger deadline passes).
                        while let Ok(data) = send_rx.try_recv() {
                            let _ = engine.send(data);
                        }
                        send_closed = true;
                        closing = true;
                        close_deadline = Some(
                            tokio::time::Instant::now() + std::time::Duration::from_secs(10),
                        );
                        let _ = engine.flush();
                        flush_output(&mut engine, &transport, &peer_addr, simulate_loss).await;
                    }
                }
            }

            // Incoming network packets — prioritized over accepting new writes so
            // ACKs (which open the send window) are processed first.
            packet = input_rx.recv() => {
                match packet {
                    Some(data) => {
                        let _ = engine.input(data);
                        // Flush after input: incoming ACKs reopen the send window,
                        // so push any backpressured queue data out now instead of
                        // waiting for the next tick (otherwise a backpressured
                        // writer is paced at one window per tick, not per RTT).
                        if let Err(e) = engine.flush() {
                            if e.is_fatal() {
                                error!(error = %e, "Engine flush after input fatal, stopping actor");
                                break;
                            }
                        }
                        flush_output(&mut engine, &transport, &peer_addr, simulate_loss).await;
                        drain_recv(&mut engine, &data_tx);
                    }
                    None => {
                        // Input channel closed — peer recv_task or listener gone
                        trace!("Input channel closed, stopping actor");
                        break;
                    }
                }
            }

            // Application data to send — only pulled while the engine's send
            // queue is below the high-water mark. When it isn't, this branch is
            // disabled, the bounded `send_rx` channel fills, and `poll_write` /
            // `KcpStream::send` block: real backpressure instead of unbounded
            // `snd_queue` growth (OOM) on a fast writer over a slow link.
            data = send_rx.recv(), if !send_closed && engine.send_queue_len() < send_high_water => {
                match data {
                    Some(data) => {
                        if let Err(e) = engine.send(data) {
                            // Should not happen: the stream chunks/validates writes
                            // below the engine's limit. Log rather than drop silently.
                            warn!(error = %e, "engine.send rejected outbound data");
                        }
                        // Absorb whatever else is already queued in the channel
                        // before flushing once: engine.send() only queues (it no
                        // longer flushes internally), so consecutive small
                        // messages share MTU-packed datagrams instead of paying
                        // one datagram + syscall each.
                        while engine.send_queue_len() < send_high_water {
                            match send_rx.try_recv() {
                                Ok(more) => {
                                    if let Err(e) = engine.send(more) {
                                        warn!(error = %e, "engine.send rejected outbound data");
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        if let Err(e) = engine.flush() {
                            if e.is_fatal() {
                                error!(error = %e, "Engine flush after send fatal, stopping actor");
                                break;
                            }
                            warn!(error = %e, "Engine flush after send failed (recoverable)");
                        }
                        flush_output(&mut engine, &transport, &peer_addr, simulate_loss).await;
                    }
                    None => {
                        // All write handles dropped — disable this branch so it
                        // can't busy-loop. The engine keeps draining in-flight
                        // data until Close or the input channel closes.
                        send_closed = true;
                    }
                }
            }
        }

        // Graceful-close drain: exit once the peer has acknowledged everything
        // we sent, or the linger deadline passes (give up on a stuck tail).
        if closing
            && (!engine.has_unsent_data()
                || close_deadline.is_some_and(|d| tokio::time::Instant::now() >= d))
        {
            let _ = engine.flush();
            flush_output(&mut engine, &transport, &peer_addr, simulate_loss).await;
            break;
        }
    }
}

/// Send all buffered output packets over the transport.
///
/// When `simulate_loss` is set, each outbound datagram is dropped with that
/// probability — wiring up `KcpConfig::simulate_packet_loss` (previously a
/// silent no-op) so the `testing()` preset actually exercises retransmission.
async fn flush_output<T: Transport>(
    engine: &mut KcpEngine,
    transport: &Arc<T>,
    peer: &T::Addr,
    simulate_loss: Option<f32>,
) {
    for buf in engine.drain_output() {
        if let Some(p) = simulate_loss {
            if random_unit_f32() < p {
                trace!("simulate_packet_loss: dropping outbound datagram");
                continue;
            }
        }
        if let Err(e) = transport.send_to(&buf, peer).await {
            trace!(error = %e, "Transport send_to failed");
        }
    }
}

/// Cheap `[0, 1)` sample from OS-seeded hashing — keeps `rand` out of the
/// runtime dependencies for the `simulate_packet_loss` test knob.
fn random_unit_f32() -> f32 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let bits = RandomState::new().build_hasher().finish();
    // Top 24 bits → a uniform fraction in [0, 1).
    (bits >> 40) as f32 / (1u64 << 24) as f32
}

/// Drain all complete application messages from the engine and forward them
/// to the user via `data_tx`.
///
/// Capacity is reserved *before* pulling a message out of the engine. Once
/// `engine.recv()` returns a message it has advanced `rcv_nxt` and the peer has
/// already been ACKed, so a message we then failed to enqueue would be lost
/// forever — a silent reliability violation. By reserving first we leave
/// undeliverable messages in the engine's receive queue, which also keeps the
/// KCP receive window honest so the peer's flow control throttles the sender
/// instead of overrunning us. Leftover messages are flushed on the next periodic
/// tick once the consumer drains `data_rx`.
fn drain_recv(engine: &mut KcpEngine, data_tx: &mpsc::Sender<Bytes>) {
    loop {
        let permit = match data_tx.try_reserve() {
            Ok(permit) => permit,
            Err(_) => break, // Channel full or closed — keep data in the engine
        };
        match engine.recv() {
            Ok(Some(msg)) => permit.send(msg),
            _ => break, // No complete message ready; release the reserved slot
        }
    }
}

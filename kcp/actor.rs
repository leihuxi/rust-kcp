//! Actor-based engine driver — owns the KcpEngine in a dedicated task,
//! communicates via channels. Zero locks on the hot path.

use crate::engine::KcpEngine;
use crate::common::KcpStats;
use crate::error::{KcpError, Result};
use crate::transport::Transport;

use bytes::Bytes;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::time::MissedTickBehavior;
use tracing::{error, trace, warn};

/// Commands sent to the engine actor.
pub(crate) enum EngineCmd {
    Send {
        data: Bytes,
        reply: oneshot::Sender<Result<()>>,
    },
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

    pub async fn send(&self, data: Bytes) -> Result<()> {
        self.request(|reply| EngineCmd::Send { data, reply })
            .await?
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
}

/// Run the engine actor loop.
///
/// - `input_rx`: raw UDP packets from recv_task (client) or listener (server).
/// - `data_tx`: assembled application messages forwarded to user reads.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_engine_actor<T: Transport>(
    mut engine: KcpEngine,
    mut cmd_rx: mpsc::Receiver<EngineCmd>,
    mut input_rx: mpsc::Receiver<Bytes>,
    data_tx: mpsc::Sender<Bytes>,
    transport: Arc<T>,
    peer_addr: T::Addr,
    update_interval_ms: u64,
    keep_alive_ms: Option<u64>,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(update_interval_ms));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // Initial update + flush + drain any pre-loaded messages
    // (server streams may have processed initial packets before spawning the actor)
    let _ = engine.update();
    flush_output(&mut engine, &transport, &peer_addr).await;
    drain_recv(&mut engine, &data_tx);

    loop {
        tokio::select! {
            biased;

            // Periodic update tick (prioritized to avoid timer starvation)
            _ = interval.tick() => {
                if let Err(e) = engine.update() {
                    if e.is_fatal() {
                        error!(error = %e, "Engine update fatal error, stopping actor");
                        break;
                    }
                    warn!(error = %e, "Engine update failed (recoverable)");
                }

                // Keep-alive
                if let Some(ka) = keep_alive_ms {
                    if engine.idle_ms() as u64 >= ka {
                        if let Err(e) = engine.keep_alive_probe() {
                            if e.is_fatal() {
                                error!(error = %e, "Keep-alive probe fatal");
                                break;
                            }
                            warn!(error = %e, "Keep-alive probe failed");
                        }
                    }
                }

                flush_output(&mut engine, &transport, &peer_addr).await;
            }

            // User commands
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(EngineCmd::Send { data, reply }) => {
                        let r = engine.send(data).map_err(KcpError::from);
                        flush_output(&mut engine, &transport, &peer_addr).await;
                        let _ = reply.send(r);
                    }
                    Some(EngineCmd::Flush { reply }) => {
                        let r = engine.flush().map_err(KcpError::from);
                        flush_output(&mut engine, &transport, &peer_addr).await;
                        let _ = reply.send(r);
                    }
                    Some(EngineCmd::Stats { reply }) => {
                        let _ = reply.send(*engine.stats());
                    }
                    Some(EngineCmd::IsAlive { reply }) => {
                        let _ = reply.send(!engine.is_dead());
                    }
                    Some(EngineCmd::Close) | None => {
                        // Graceful shutdown: flush remaining data
                        let _ = engine.flush();
                        flush_output(&mut engine, &transport, &peer_addr).await;
                        break;
                    }
                }
            }

            // Incoming network packets
            packet = input_rx.recv() => {
                match packet {
                    Some(data) => {
                        let _ = engine.input(data);
                        flush_output(&mut engine, &transport, &peer_addr).await;
                        drain_recv(&mut engine, &data_tx);
                    }
                    None => {
                        // Input channel closed — peer recv_task or listener gone
                        trace!("Input channel closed, stopping actor");
                        break;
                    }
                }
            }
        }
    }
}

/// Send all buffered output packets over the transport.
async fn flush_output<T: Transport>(engine: &mut KcpEngine, transport: &Arc<T>, peer: &T::Addr) {
    for buf in engine.drain_output() {
        if let Err(e) = transport.send_to(&buf, peer).await {
            trace!(error = %e, "Transport send_to failed");
        }
    }
}

/// Drain all complete application messages from the engine and forward them
/// to the user via `data_tx`.
fn drain_recv(engine: &mut KcpEngine, data_tx: &mpsc::Sender<Bytes>) {
    while let Ok(Some(msg)) = engine.recv() {
        if data_tx.try_send(msg).is_err() {
            break; // Channel full or closed
        }
    }
}

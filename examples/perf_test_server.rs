//! High-performance KCP test server for validating optimizations
//!
//! This server demonstrates the optimized KCP implementation with:
//! - Ultra-low latency configuration
//! - Real-time performance monitoring  
//! - Concurrent connection handling
//! - Comprehensive metrics reporting

use kcp_tokio::{async_kcp::KcpListener, metrics, KcpConfig};
use std::time::{Duration, Instant};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    select, signal,
    sync::mpsc,
    time::interval,
};
use tracing::{error, info, warn};

#[derive(Debug, Clone)]
struct ServerStats {
    connections: usize,
    total_messages: u64,
    total_bytes: u64,
    start_time: Instant,
}

impl ServerStats {
    fn new() -> Self {
        Self {
            connections: 0,
            total_messages: 0,
            total_bytes: 0,
            start_time: Instant::now(),
        }
    }

    fn uptime(&self) -> Duration {
        Instant::now() - self.start_time
    }

    fn throughput_mbps(&self) -> f64 {
        let elapsed = self.uptime().as_secs_f64();
        if elapsed > 0.0 {
            (self.total_bytes as f64 * 8.0) / (elapsed * 1_000_000.0)
        } else {
            0.0
        }
    }

    fn message_rate(&self) -> f64 {
        let elapsed = self.uptime().as_secs_f64();
        if elapsed > 0.0 {
            self.total_messages as f64 / elapsed
        } else {
            0.0
        }
    }
}

async fn handle_client(
    mut stream: kcp_tokio::async_kcp::KcpStream,
    client_id: u32,
    stats_tx: mpsc::UnboundedSender<(u64, u64)>,
) {
    let peer_addr = stream.peer_addr();
    info!("Client {} connected from {}", client_id, peer_addr);

    let mut buffer = [0u8; 8192];
    let mut message_count = 0u64;
    let mut byte_count = 0u64;
    let start_time = Instant::now();

    loop {
        match stream.read(&mut buffer).await {
            Ok(0) => {
                info!("Client {} disconnected", client_id);
                break;
            }
            Ok(n) => {
                message_count += 1;
                byte_count += n as u64;

                // Echo the message back with timestamp for RTT measurement
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64;

                let mut response = Vec::with_capacity(n + 8);
                response.extend_from_slice(&timestamp.to_le_bytes());
                response.extend_from_slice(&buffer[..n]);

                if let Err(e) = stream.write_all(&response).await {
                    error!("Failed to write to client {}: {}", client_id, e);
                    break;
                }

                // Send stats update every 1000 messages
                if message_count % 1000 == 0 {
                    let _ = stats_tx.send((message_count, byte_count));

                    let elapsed = start_time.elapsed().as_secs_f64();
                    let msg_rate = message_count as f64 / elapsed;
                    let throughput = (byte_count as f64 * 8.0) / (elapsed * 1_000_000.0);

                    info!(
                        "Client {}: {} msgs, {:.2} MB, {:.0} msg/s, {:.2} Mbps",
                        client_id,
                        message_count,
                        byte_count as f64 / 1_000_000.0,
                        msg_rate,
                        throughput
                    );
                }
            }
            Err(e) => {
                error!("Error reading from client {}: {}", client_id, e);
                break;
            }
        }
    }

    // Send final stats
    let _ = stats_tx.send((message_count, byte_count));

    let elapsed = start_time.elapsed().as_secs_f64();
    info!(
        "Client {} session ended: {} messages, {:.2} MB in {:.2}s",
        client_id,
        message_count,
        byte_count as f64 / 1_000_000.0,
        elapsed
    );
}

async fn stats_monitor(mut stats_rx: mpsc::UnboundedReceiver<(u64, u64)>) {
    let mut total_stats = ServerStats::new();
    let mut interval = interval(Duration::from_secs(5));

    loop {
        select! {
            _ = interval.tick() => {
                // Print periodic stats
                let global_metrics = metrics::global_metrics().snapshot();
                let buffer_stats = kcp_tokio::common::buffer_pool_stats();

                info!("=== Server Performance Stats ===");
                info!("Active connections: {}", global_metrics.active_connections);
                info!("Messages processed: {} ({:.0}/s)",
                      total_stats.total_messages, total_stats.message_rate());
                info!("Data processed: {:.2} MB ({:.2} Mbps)",
                      total_stats.total_bytes as f64 / 1_000_000.0, total_stats.throughput_mbps());
                info!("Uptime: {:.1}s", total_stats.uptime().as_secs_f64());

                info!("Buffer pool stats:");
                for (name, hits, size) in buffer_stats {
                    info!("  {}: {} buffers cached, {} hits", name, size, hits);
                }

                if global_metrics.total_packets_sent > 0 {
                    let loss_rate = (global_metrics.total_retransmissions as f64 /
                                   global_metrics.total_packets_sent as f64) * 100.0;
                    info!("Packet loss rate: {:.3}%", loss_rate);
                }

                info!("====================================");
            }

            Some((msgs, bytes)) = stats_rx.recv() => {
                total_stats.total_messages += msgs;
                total_stats.total_bytes += bytes;
            }

            else => break,
        }
    }
}

async fn run_server(addr: &str, config: KcpConfig) -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting KCP performance test server on {}", addr);
    info!("Configuration: {:?}", config);

    let addr: std::net::SocketAddr = addr.parse()?;
    let mut listener = KcpListener::bind(addr, config).await?;
    info!("Server listening on {}", addr);

    let (stats_tx, stats_rx) = mpsc::unbounded_channel();

    // Start stats monitoring task
    let stats_task = tokio::spawn(stats_monitor(stats_rx));

    let mut client_id = 0u32;
    let mut shutdown = Box::pin(signal::ctrl_c());

    loop {
        select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, addr)) => {
                        client_id += 1;
                        info!("New connection from {}", addr);

                        let stats_tx_clone = stats_tx.clone();
                        tokio::spawn(handle_client(stream, client_id, stats_tx_clone));

                        // Update global metrics
                        metrics::global_metrics().connection_created();
                    }
                    Err(e) => {
                        error!("Failed to accept connection: {}", e);
                    }
                }
            }

            _ = &mut shutdown => {
                info!("Received shutdown signal");
                break;
            }
        }
    }

    stats_task.abort();
    info!("Server shutdown complete");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .with_thread_ids(true)
        .compact()
        .init();

    let args: Vec<String> = std::env::args().collect();
    let addr = args.get(1).map(|s| s.as_str()).unwrap_or("127.0.0.1:12345");

    let mode = args.get(2).map(|s| s.as_str()).unwrap_or("gaming");

    let config = match mode {
        "normal" => KcpConfig::default(),
        "fast" => KcpConfig::new().fast_mode().window_size(128, 128),
        "turbo" => KcpConfig::new().turbo_mode().window_size(256, 256),
        "gaming" => KcpConfig::gaming(),
        "file_transfer" => KcpConfig::file_transfer(),
        _ => {
            warn!("Unknown mode '{}', using gaming mode", mode);
            KcpConfig::gaming()
        }
    };

    info!("Using {} mode configuration", mode);

    run_server(addr, config).await
}

//! High-performance KCP test client for validating optimizations
//!
//! This client demonstrates the optimized KCP implementation with:
//! - Latency measurement and analysis
//! - Throughput testing with various message sizes
//! - Concurrent connection testing
//! - Real-time performance monitoring

use kcp_tokio::{metrics, KcpConfig, KcpStream};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    select,
    time::{interval, sleep},
};
use tracing::{error, info, warn};

#[derive(Debug, Clone)]
struct LatencyStats {
    samples: VecDeque<u32>,
    min: u32,
    max: u32,
    sum: u64,
    count: u64,
}

impl LatencyStats {
    fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(10000),
            min: u32::MAX,
            max: 0,
            sum: 0,
            count: 0,
        }
    }

    fn add_sample(&mut self, latency: u32) {
        self.samples.push_back(latency);
        if self.samples.len() > 10000 {
            if let Some(old) = self.samples.pop_front() {
                self.sum -= old as u64;
                self.count -= 1;
            }
        }

        self.sum += latency as u64;
        self.count += 1;
        self.min = self.min.min(latency);
        self.max = self.max.max(latency);
    }

    fn average(&self) -> f64 {
        if self.count > 0 {
            self.sum as f64 / self.count as f64
        } else {
            0.0
        }
    }

    fn percentile(&self, p: f64) -> u32 {
        if self.samples.is_empty() {
            return 0;
        }

        let mut sorted: Vec<u32> = self.samples.iter().copied().collect();
        sorted.sort_unstable();

        let index = ((sorted.len() as f64 - 1.0) * p / 100.0) as usize;
        sorted[index.min(sorted.len() - 1)]
    }

    fn jitter(&self) -> f64 {
        if self.samples.len() < 2 {
            return 0.0;
        }

        let avg = self.average();
        let variance: f64 = self
            .samples
            .iter()
            .map(|&x| (x as f64 - avg).powi(2))
            .sum::<f64>()
            / self.samples.len() as f64;

        variance.sqrt()
    }
}

#[derive(Debug, Clone)]
struct ClientStats {
    messages_sent: u64,
    messages_received: u64,
    bytes_sent: u64,
    bytes_received: u64,
    start_time: Instant,
    latency: LatencyStats,
    errors: u64,
}

impl ClientStats {
    fn new() -> Self {
        Self {
            messages_sent: 0,
            messages_received: 0,
            bytes_sent: 0,
            bytes_received: 0,
            start_time: Instant::now(),
            latency: LatencyStats::new(),
            errors: 0,
        }
    }

    fn uptime(&self) -> Duration {
        Instant::now() - self.start_time
    }

    fn message_rate(&self) -> f64 {
        let elapsed = self.uptime().as_secs_f64();
        if elapsed > 0.0 {
            self.messages_sent as f64 / elapsed
        } else {
            0.0
        }
    }

    fn throughput_mbps(&self) -> f64 {
        let elapsed = self.uptime().as_secs_f64();
        if elapsed > 0.0 {
            (self.bytes_sent as f64 * 8.0) / (elapsed * 1_000_000.0)
        } else {
            0.0
        }
    }
}

async fn latency_test(
    addr: &str,
    config: KcpConfig,
    duration: Duration,
    message_size: usize,
    message_rate: u64, // messages per second
) -> Result<ClientStats, Box<dyn std::error::Error>> {
    info!(
        "Starting latency test: {}B messages at {} msg/s for {}s",
        message_size,
        message_rate,
        duration.as_secs()
    );

    let addr: std::net::SocketAddr = addr.parse()?;
    let mut stream = KcpStream::connect(addr, config).await?;
    let mut stats = ClientStats::new();

    let mut send_interval = interval(Duration::from_millis(1000 / message_rate));
    let end_time = Instant::now() + duration;

    let test_data = vec![0xAAu8; message_size];
    let mut receive_buffer = [0u8; 16384];
    let mut pending_timestamps = VecDeque::new();

    loop {
        let now = Instant::now();
        if now >= end_time {
            break;
        }

        select! {
            _ = send_interval.tick() => {
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64;

                pending_timestamps.push_back(timestamp);

                let mut message = Vec::with_capacity(8 + test_data.len());
                message.extend_from_slice(&timestamp.to_le_bytes());
                message.extend_from_slice(&test_data);

                match stream.write_all(&message).await {
                    Ok(_) => {
                        stats.messages_sent += 1;
                        stats.bytes_sent += message.len() as u64;
                    }
                    Err(e) => {
                        error!("Send error: {}", e);
                        stats.errors += 1;
                    }
                }
            }

            result = stream.read(&mut receive_buffer) => {
                match result {
                    Ok(n) if n >= 8 => {
                        let recv_timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64;

                        // Extract the original timestamp
                        let sent_timestamp = u64::from_le_bytes([
                            receive_buffer[0], receive_buffer[1], receive_buffer[2], receive_buffer[3],
                            receive_buffer[4], receive_buffer[5], receive_buffer[6], receive_buffer[7],
                        ]);

                        let rtt = (recv_timestamp - sent_timestamp) as u32;
                        stats.latency.add_sample(rtt);
                        stats.messages_received += 1;
                        stats.bytes_received += n as u64;
                    }
                    Ok(_) => {
                        warn!("Received short message");
                        stats.errors += 1;
                    }
                    Err(e) => {
                        error!("Receive error: {}", e);
                        stats.errors += 1;
                    }
                }
            }
        }
    }

    // Wait a bit for remaining responses
    let grace_period = Duration::from_millis(100);
    let grace_end = Instant::now() + grace_period;

    while Instant::now() < grace_end {
        if let Ok(n) =
            tokio::time::timeout(Duration::from_millis(10), stream.read(&mut receive_buffer)).await
        {
            match n {
                Ok(n) if n >= 8 => {
                    let recv_timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64;

                    let sent_timestamp = u64::from_le_bytes([
                        receive_buffer[0],
                        receive_buffer[1],
                        receive_buffer[2],
                        receive_buffer[3],
                        receive_buffer[4],
                        receive_buffer[5],
                        receive_buffer[6],
                        receive_buffer[7],
                    ]);

                    let rtt = (recv_timestamp - sent_timestamp) as u32;
                    stats.latency.add_sample(rtt);
                    stats.messages_received += 1;
                    stats.bytes_received += n as u64;
                }
                _ => break,
            }
        } else {
            break;
        }
    }

    Ok(stats)
}

async fn throughput_test(
    addr: &str,
    config: KcpConfig,
    duration: Duration,
    message_size: usize,
) -> Result<ClientStats, Box<dyn std::error::Error>> {
    info!(
        "Starting throughput test: {}B messages for {}s (unlimited rate)",
        message_size,
        duration.as_secs()
    );

    let addr: std::net::SocketAddr = addr.parse()?;
    let mut stream = KcpStream::connect(addr, config).await?;
    let mut stats = ClientStats::new();

    let end_time = Instant::now() + duration;
    let test_data = vec![0xBBu8; message_size];
    let mut receive_buffer = [0u8; 16384];

    let mut last_stats_print = Instant::now();

    loop {
        let now = Instant::now();
        if now >= end_time {
            break;
        }

        // Send as fast as possible
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let mut message = Vec::with_capacity(8 + test_data.len());
        message.extend_from_slice(&timestamp.to_le_bytes());
        message.extend_from_slice(&test_data);

        // Send message
        match stream.write_all(&message).await {
            Ok(_) => {
                stats.messages_sent += 1;
                stats.bytes_sent += message.len() as u64;
            }
            Err(e) => {
                error!("Send error: {}", e);
                stats.errors += 1;
                sleep(Duration::from_millis(1)).await;
                continue;
            }
        }

        // Try to read response (non-blocking)
        match tokio::time::timeout(Duration::from_millis(1), stream.read(&mut receive_buffer)).await
        {
            Ok(Ok(n)) if n >= 8 => {
                stats.messages_received += 1;
                stats.bytes_received += n as u64;
            }
            Ok(Ok(_)) => {
                stats.errors += 1;
            }
            Ok(Err(e)) => {
                error!("Receive error: {}", e);
                stats.errors += 1;
            }
            Err(_) => {
                // Timeout - continue sending
            }
        }

        // Print stats every 2 seconds
        if now.duration_since(last_stats_print) >= Duration::from_secs(2) {
            info!(
                "Progress: {} msgs sent ({:.1} msg/s), {:.2} Mbps",
                stats.messages_sent,
                stats.message_rate(),
                stats.throughput_mbps()
            );
            last_stats_print = now;
        }
    }

    Ok(stats)
}

fn print_test_results(test_name: &str, stats: &ClientStats) {
    info!("=== {} Results ===", test_name);
    info!("Test duration: {:.2}s", stats.uptime().as_secs_f64());
    info!(
        "Messages: {} sent, {} received",
        stats.messages_sent, stats.messages_received
    );
    info!(
        "Data: {:.2} MB sent, {:.2} MB received",
        stats.bytes_sent as f64 / 1_000_000.0,
        stats.bytes_received as f64 / 1_000_000.0
    );
    info!("Message rate: {:.1} msg/s", stats.message_rate());
    info!("Throughput: {:.2} Mbps", stats.throughput_mbps());
    info!("Errors: {}", stats.errors);

    if stats.latency.count > 0 {
        info!("Latency stats:");
        info!("  Average: {:.2}ms", stats.latency.average());
        info!(
            "  Min/Max: {}ms / {}ms",
            stats.latency.min, stats.latency.max
        );
        info!(
            "  P50: {}ms, P95: {}ms, P99: {}ms",
            stats.latency.percentile(50.0),
            stats.latency.percentile(95.0),
            stats.latency.percentile(99.0)
        );
        info!("  Jitter: {:.2}ms", stats.latency.jitter());
        info!("  Samples: {}", stats.latency.count);
    }

    let success_rate = if stats.messages_sent > 0 {
        (stats.messages_received as f64 / stats.messages_sent as f64) * 100.0
    } else {
        0.0
    };
    info!("Success rate: {:.1}%", success_rate);
    info!("=====================================");
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
    let test_type = args.get(3).map(|s| s.as_str()).unwrap_or("latency");

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

    info!("Connecting to {} using {} mode", addr, mode);
    info!("Configuration: {:?}", config);

    match test_type {
        "latency" => {
            info!("Running latency tests...");

            // Low-frequency latency test (like gaming heartbeats)
            let stats = latency_test(addr, config.clone(), Duration::from_secs(10), 64, 20).await?;
            print_test_results("Latency Test (64B @ 20Hz)", &stats);

            sleep(Duration::from_secs(1)).await;

            // Medium-frequency latency test (like game state updates)
            let stats =
                latency_test(addr, config.clone(), Duration::from_secs(10), 512, 60).await?;
            print_test_results("Latency Test (512B @ 60Hz)", &stats);
        }

        "throughput" => {
            info!("Running throughput tests...");

            // Small message throughput
            let stats = throughput_test(addr, config.clone(), Duration::from_secs(10), 64).await?;
            print_test_results("Throughput Test (64B)", &stats);

            sleep(Duration::from_secs(1)).await;

            // Large message throughput
            let stats =
                throughput_test(addr, config.clone(), Duration::from_secs(10), 1024).await?;
            print_test_results("Throughput Test (1KB)", &stats);
        }

        "mixed" => {
            info!("Running mixed performance tests...");

            let stats = latency_test(addr, config.clone(), Duration::from_secs(5), 64, 20).await?;
            print_test_results("Mixed Test - Latency (64B)", &stats);

            sleep(Duration::from_secs(1)).await;

            let stats = throughput_test(addr, config.clone(), Duration::from_secs(5), 512).await?;
            print_test_results("Mixed Test - Throughput (512B)", &stats);
        }

        _ => {
            error!(
                "Unknown test type '{}'. Use: latency, throughput, or mixed",
                test_type
            );
            return Ok(());
        }
    }

    // Show global metrics
    let global_metrics = metrics::global_metrics().snapshot();
    info!(
        "Global metrics: {}",
        metrics::format_metrics(&global_metrics)
    );

    info!("Test completed successfully!");
    Ok(())
}

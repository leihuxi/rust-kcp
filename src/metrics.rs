//! Performance metrics and monitoring for KCP connections

use crate::common::KcpStats;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Global performance metrics collector
#[derive(Debug)]
pub struct GlobalMetrics {
    /// Total connections created
    pub connections_created: AtomicU64,
    /// Active connections
    pub active_connections: AtomicUsize,
    /// Total bytes sent across all connections
    pub total_bytes_sent: AtomicU64,
    /// Total bytes received across all connections
    pub total_bytes_received: AtomicU64,
    /// Total packets sent
    pub total_packets_sent: AtomicU64,
    /// Total packets received
    pub total_packets_received: AtomicU64,
    /// Total retransmissions
    pub total_retransmissions: AtomicU64,
    /// Buffer pool statistics
    pub buffer_pool_hits: AtomicU64,
    /// Buffer pool misses
    pub buffer_pool_misses: AtomicU64,
}

impl Default for GlobalMetrics {
    fn default() -> Self {
        Self {
            connections_created: AtomicU64::new(0),
            active_connections: AtomicUsize::new(0),
            total_bytes_sent: AtomicU64::new(0),
            total_bytes_received: AtomicU64::new(0),
            total_packets_sent: AtomicU64::new(0),
            total_packets_received: AtomicU64::new(0),
            total_retransmissions: AtomicU64::new(0),
            buffer_pool_hits: AtomicU64::new(0),
            buffer_pool_misses: AtomicU64::new(0),
        }
    }
}

impl GlobalMetrics {
    /// Record a new connection
    pub fn connection_created(&self) {
        self.connections_created.fetch_add(1, Ordering::Relaxed);
        self.active_connections.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a connection closure
    pub fn connection_closed(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    /// Update metrics from connection stats
    pub fn update_from_stats(&self, stats: &KcpStats) {
        self.total_bytes_sent
            .store(stats.bytes_sent, Ordering::Relaxed);
        self.total_bytes_received
            .store(stats.bytes_received, Ordering::Relaxed);
        self.total_packets_sent
            .store(stats.packets_sent, Ordering::Relaxed);
        self.total_packets_received
            .store(stats.packets_received, Ordering::Relaxed);
        self.total_retransmissions
            .store(stats.retransmissions, Ordering::Relaxed);
    }

    /// Record buffer pool hit
    pub fn buffer_pool_hit(&self) {
        self.buffer_pool_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record buffer pool miss
    pub fn buffer_pool_miss(&self) {
        self.buffer_pool_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Get current metrics snapshot
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            connections_created: self.connections_created.load(Ordering::Relaxed),
            active_connections: self.active_connections.load(Ordering::Relaxed),
            total_bytes_sent: self.total_bytes_sent.load(Ordering::Relaxed),
            total_bytes_received: self.total_bytes_received.load(Ordering::Relaxed),
            total_packets_sent: self.total_packets_sent.load(Ordering::Relaxed),
            total_packets_received: self.total_packets_received.load(Ordering::Relaxed),
            total_retransmissions: self.total_retransmissions.load(Ordering::Relaxed),
            buffer_pool_hits: self.buffer_pool_hits.load(Ordering::Relaxed),
            buffer_pool_misses: self.buffer_pool_misses.load(Ordering::Relaxed),
        }
    }
}

/// Snapshot of metrics at a point in time
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub connections_created: u64,
    pub active_connections: usize,
    pub total_bytes_sent: u64,
    pub total_bytes_received: u64,
    pub total_packets_sent: u64,
    pub total_packets_received: u64,
    pub total_retransmissions: u64,
    pub buffer_pool_hits: u64,
    pub buffer_pool_misses: u64,
}

impl MetricsSnapshot {
    /// Calculate buffer pool hit rate
    pub fn buffer_pool_hit_rate(&self) -> f64 {
        let total = self.buffer_pool_hits + self.buffer_pool_misses;
        if total == 0 {
            0.0
        } else {
            self.buffer_pool_hits as f64 / total as f64
        }
    }

    /// Calculate packet loss rate
    pub fn packet_loss_rate(&self) -> f64 {
        if self.total_packets_sent == 0 {
            0.0
        } else {
            self.total_retransmissions as f64 / self.total_packets_sent as f64
        }
    }

    /// Calculate total throughput in bytes per second
    pub fn throughput_bps(&self, duration: Duration) -> f64 {
        let total_bytes = self.total_bytes_sent + self.total_bytes_received;
        total_bytes as f64 / duration.as_secs_f64()
    }
}

/// Performance monitoring for individual connections
#[derive(Debug)]
pub struct ConnectionMonitor {
    start_time: Instant,
    last_update: RwLock<Instant>,
    peak_rtt: AtomicU64,
    min_rtt: AtomicU64,
    rtt_samples: Arc<RwLock<Vec<u32>>>,
}

impl Default for ConnectionMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionMonitor {
    /// Create a new connection monitor
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            start_time: now,
            last_update: RwLock::new(now),
            peak_rtt: AtomicU64::new(0),
            min_rtt: AtomicU64::new(u64::MAX),
            rtt_samples: Arc::new(RwLock::new(Vec::with_capacity(1000))),
        }
    }

    /// Update RTT statistics
    pub async fn update_rtt(&self, rtt: u32) {
        let rtt_u64 = rtt as u64;

        // Update peak RTT
        let mut current_peak = self.peak_rtt.load(Ordering::Relaxed);
        while rtt_u64 > current_peak {
            match self.peak_rtt.compare_exchange_weak(
                current_peak,
                rtt_u64,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(x) => current_peak = x,
            }
        }

        // Update minimum RTT
        let mut current_min = self.min_rtt.load(Ordering::Relaxed);
        while rtt_u64 < current_min {
            match self.min_rtt.compare_exchange_weak(
                current_min,
                rtt_u64,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(x) => current_min = x,
            }
        }

        // Store RTT sample (keep last 1000 samples)
        let mut samples = self.rtt_samples.write().await;
        if samples.len() >= 1000 {
            samples.remove(0);
        }
        samples.push(rtt);

        // Update last activity
        *self.last_update.write().await = Instant::now();
    }

    /// Get connection uptime
    pub fn uptime(&self) -> Duration {
        Instant::now().duration_since(self.start_time)
    }

    /// Get time since last activity
    pub async fn idle_time(&self) -> Duration {
        let last_update = *self.last_update.read().await;
        Instant::now().duration_since(last_update)
    }

    /// Get RTT statistics
    pub async fn rtt_stats(&self) -> RttStats {
        let samples = self.rtt_samples.read().await;
        let peak = self.peak_rtt.load(Ordering::Relaxed) as u32;
        let min = {
            let min_val = self.min_rtt.load(Ordering::Relaxed);
            if min_val == u64::MAX {
                0
            } else {
                min_val as u32
            }
        };

        let (avg, jitter) = if samples.is_empty() {
            (0, 0)
        } else {
            let sum: u32 = samples.iter().sum();
            let avg = sum / samples.len() as u32;

            // Calculate jitter (average deviation from mean)
            let variance: f64 = samples
                .iter()
                .map(|&rtt| (rtt as f64 - avg as f64).powi(2))
                .sum::<f64>()
                / samples.len() as f64;
            let jitter = variance.sqrt() as u32;

            (avg, jitter)
        };

        RttStats {
            current: samples.last().copied().unwrap_or(0),
            average: avg,
            minimum: min,
            maximum: peak,
            jitter,
            sample_count: samples.len(),
        }
    }
}

/// RTT statistics for a connection
#[derive(Debug, Clone)]
pub struct RttStats {
    pub current: u32,
    pub average: u32,
    pub minimum: u32,
    pub maximum: u32,
    pub jitter: u32,
    pub sample_count: usize,
}

lazy_static::lazy_static! {
    /// Global metrics instance
    pub static ref GLOBAL_METRICS: GlobalMetrics = GlobalMetrics::default();
}

/// Get global metrics
pub fn global_metrics() -> &'static GlobalMetrics {
    &GLOBAL_METRICS
}

/// Format metrics for human-readable display
pub fn format_metrics(snapshot: &MetricsSnapshot) -> String {
    format!(
        "KCP Metrics:\n\
         Connections: {} created, {} active\n\
         Traffic: {} bytes sent, {} bytes received\n\
         Packets: {} sent, {} received\n\
         Retransmissions: {} (loss rate: {:.2}%)\n\
         Buffer Pool: {:.1}% hit rate ({} hits, {} misses)",
        snapshot.connections_created,
        snapshot.active_connections,
        snapshot.total_bytes_sent,
        snapshot.total_bytes_received,
        snapshot.total_packets_sent,
        snapshot.total_packets_received,
        snapshot.total_retransmissions,
        snapshot.packet_loss_rate() * 100.0,
        snapshot.buffer_pool_hit_rate() * 100.0,
        snapshot.buffer_pool_hits,
        snapshot.buffer_pool_misses,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_global_metrics() {
        let metrics = GlobalMetrics::default();

        metrics.connection_created();
        assert_eq!(metrics.active_connections.load(Ordering::Relaxed), 1);

        metrics.connection_closed();
        assert_eq!(metrics.active_connections.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_connection_monitor() {
        let monitor = ConnectionMonitor::new();

        monitor.update_rtt(100).await;
        monitor.update_rtt(150).await;
        monitor.update_rtt(75).await;

        let stats = monitor.rtt_stats().await;
        assert_eq!(stats.minimum, 75);
        assert_eq!(stats.maximum, 150);
        assert_eq!(stats.sample_count, 3);
    }
}

//! Performance metrics and monitoring for KCP connections

use crate::common::KcpStats;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

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

    /// Update metrics from connection stats (accumulates, not overwrites)
    pub fn update_from_stats(&self, stats: &KcpStats) {
        self.total_bytes_sent
            .fetch_add(stats.bytes_sent, Ordering::Relaxed);
        self.total_bytes_received
            .fetch_add(stats.bytes_received, Ordering::Relaxed);
        self.total_packets_sent
            .fetch_add(stats.packets_sent, Ordering::Relaxed);
        self.total_packets_received
            .fetch_add(stats.packets_received, Ordering::Relaxed);
        self.total_retransmissions
            .fetch_add(stats.retransmissions, Ordering::Relaxed);
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
}

impl MetricsSnapshot {
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

/// Global metrics instance
pub static GLOBAL_METRICS: std::sync::LazyLock<GlobalMetrics> =
    std::sync::LazyLock::new(GlobalMetrics::default);

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
         Retransmissions: {} (loss rate: {:.2}%)",
        snapshot.connections_created,
        snapshot.active_connections,
        snapshot.total_bytes_sent,
        snapshot.total_bytes_received,
        snapshot.total_packets_sent,
        snapshot.total_packets_received,
        snapshot.total_retransmissions,
        snapshot.packet_loss_rate() * 100.0,
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
}

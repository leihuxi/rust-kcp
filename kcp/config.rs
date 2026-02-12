//! Configuration types for KCP.
//!
//! [`KcpConfig`] extends the core [`NodeDelayConfig`] with transport-level
//! settings (timeouts, socket buffer sizes, listener limits, etc.).

use crate::error::{KcpError, Result};
use std::net::SocketAddr;
use std::time::Duration;

// Re-export from kcp-core so users see a single NodeDelayConfig type.
pub use kcp_core::config::NodeDelayConfig;

// ── KcpConfig ───────────────────────────────────────────────────────────

/// Full KCP configuration — protocol settings + transport / runtime settings.
#[derive(Debug, Clone)]
pub struct KcpConfig {
    // Protocol settings (forwarded to kcp-core engine)
    pub mtu: u32,
    pub snd_wnd: u32,
    pub rcv_wnd: u32,
    pub nodelay: NodeDelayConfig,
    pub max_retries: u32,
    pub stream_mode: bool,

    // Transport / runtime settings (used only by kcp-tokio)
    pub connect_timeout: Duration,
    pub keep_alive: Option<Duration>,
    pub socket_buffer_size: Option<usize>,
    pub simulate_packet_loss: Option<f32>,
    pub max_pending_connections: usize,
    pub cleanup_interval: Duration,
}

impl Default for KcpConfig {
    fn default() -> Self {
        Self {
            mtu: 1400,
            snd_wnd: 32,
            rcv_wnd: 128,
            nodelay: NodeDelayConfig::normal(),
            max_retries: 20,
            stream_mode: false,
            connect_timeout: Duration::from_secs(10),
            keep_alive: Some(Duration::from_secs(30)),
            socket_buffer_size: None,
            simulate_packet_loss: None,
            max_pending_connections: 256,
            cleanup_interval: Duration::from_secs(30),
        }
    }
}

/// Extracts the 6 protocol-only fields that `KcpEngine` reads.
impl From<KcpConfig> for kcp_core::KcpCoreConfig {
    fn from(c: KcpConfig) -> Self {
        Self {
            mtu: c.mtu,
            snd_wnd: c.snd_wnd,
            rcv_wnd: c.rcv_wnd,
            nodelay: c.nodelay,
            max_retries: c.max_retries,
            stream_mode: c.stream_mode,
        }
    }
}

// ── Builder methods ─────────────────────────────────────────────────────

impl KcpConfig {
    pub fn new() -> Self {
        Self::default()
    }

    // -- Protocol tuning --

    pub fn mtu(mut self, mtu: u32) -> Self {
        self.mtu = mtu;
        self
    }

    pub fn send_window(mut self, wnd: u32) -> Self {
        self.snd_wnd = wnd;
        self
    }

    pub fn recv_window(mut self, wnd: u32) -> Self {
        self.rcv_wnd = wnd;
        self
    }

    pub fn window_size(mut self, snd_wnd: u32, rcv_wnd: u32) -> Self {
        self.snd_wnd = snd_wnd;
        self.rcv_wnd = rcv_wnd;
        self
    }

    pub fn normal_mode(mut self) -> Self {
        self.nodelay = NodeDelayConfig::normal();
        self
    }

    pub fn fast_mode(mut self) -> Self {
        self.nodelay = NodeDelayConfig::fast();
        self
    }

    pub fn turbo_mode(mut self) -> Self {
        self.nodelay = NodeDelayConfig::turbo();
        self
    }

    pub fn nodelay_config(mut self, config: NodeDelayConfig) -> Self {
        self.nodelay = config;
        self
    }

    pub fn max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    pub fn stream_mode(mut self, enabled: bool) -> Self {
        self.stream_mode = enabled;
        self
    }

    // -- Transport / runtime tuning --

    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    pub fn keep_alive(mut self, interval: Option<Duration>) -> Self {
        self.keep_alive = interval;
        self
    }

    pub fn socket_buffer_size(mut self, size: usize) -> Self {
        self.socket_buffer_size = Some(size);
        self
    }

    pub fn simulate_packet_loss(mut self, loss_rate: f32) -> Self {
        if (0.0..=1.0).contains(&loss_rate) {
            self.simulate_packet_loss = Some(loss_rate);
        }
        self
    }

    // -- Validation --

    pub fn validate(&self) -> Result<()> {
        if self.mtu < 64 || self.mtu > 65535 {
            return Err(KcpError::config("MTU must be between 64 and 65535"));
        }
        if self.snd_wnd == 0 || self.rcv_wnd == 0 {
            return Err(KcpError::config("Window sizes must be greater than 0"));
        }
        if self.nodelay.interval == 0 {
            return Err(KcpError::config("Update interval must be greater than 0"));
        }
        if self.max_retries == 0 {
            return Err(KcpError::config("Max retries must be greater than 0"));
        }
        Ok(())
    }

    // -- Convenience connect/listen --

    #[cfg(feature = "tokio")]
    pub async fn connect<A: Into<SocketAddr>>(
        self,
        addr: A,
    ) -> Result<crate::stream::KcpStream<crate::transport::UdpTransport>> {
        self.validate()?;
        crate::stream::KcpStream::connect(addr.into(), self).await
    }

    #[cfg(feature = "tokio")]
    pub async fn listen<A: Into<SocketAddr>>(
        self,
        addr: A,
    ) -> Result<crate::listener::KcpListener<crate::transport::UdpTransport>> {
        self.validate()?;
        crate::listener::KcpListener::bind(addr.into(), self).await
    }
}

// ── Presets ──────────────────────────────────────────────────────────────

impl KcpConfig {
    pub fn gaming() -> Self {
        Self::default()
            .nodelay_config(NodeDelayConfig::gaming())
            .window_size(64, 128)
            .mtu(1200)
            .connect_timeout(Duration::from_secs(3))
            .keep_alive(Some(Duration::from_secs(10)))
    }

    pub fn file_transfer() -> Self {
        Self::default()
            .normal_mode()
            .window_size(256, 256)
            .mtu(1400)
            .stream_mode(true)
            .connect_timeout(Duration::from_secs(30))
            .keep_alive(Some(Duration::from_secs(60)))
    }

    pub fn realtime() -> Self {
        Self::default()
            .fast_mode()
            .window_size(64, 64)
            .mtu(1200)
            .connect_timeout(Duration::from_secs(3))
            .keep_alive(Some(Duration::from_secs(10)))
    }

    pub fn testing(packet_loss: f32) -> Self {
        Self::default()
            .fast_mode()
            .simulate_packet_loss(packet_loss)
            .connect_timeout(Duration::from_secs(5))
    }
}

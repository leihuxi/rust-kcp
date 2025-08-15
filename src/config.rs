//! Configuration types for KCP

use crate::error::{KcpError, Result};
use std::net::SocketAddr;
use std::time::Duration;

/// KCP configuration builder
#[derive(Debug, Clone)]
pub struct KcpConfig {
    /// Maximum transmission unit
    pub mtu: u32,
    /// Send window size
    pub snd_wnd: u32,
    /// Receive window size
    pub rcv_wnd: u32,
    /// Node delay configuration
    pub nodelay: NodeDelayConfig,
    /// Connection timeout
    pub connect_timeout: Duration,
    /// Keep-alive interval
    pub keep_alive: Option<Duration>,
    /// Maximum retransmissions before giving up
    pub max_retries: u32,
    /// Enable stream mode (no message boundaries)
    pub stream_mode: bool,
    /// Buffer size for UDP socket
    pub socket_buffer_size: Option<usize>,
    /// Enable packet loss simulation (for testing)
    pub simulate_packet_loss: Option<f32>,
}

/// Node delay configuration for different performance modes
#[derive(Debug, Clone)]
pub struct NodeDelayConfig {
    /// Enable no-delay mode
    pub nodelay: bool,
    /// Internal update interval in milliseconds
    pub interval: u32,
    /// Fast resend threshold
    pub resend: u32,
    /// Disable congestion control
    pub no_congestion_control: bool,
}

impl Default for KcpConfig {
    fn default() -> Self {
        Self {
            mtu: 1400,
            snd_wnd: 32,
            rcv_wnd: 128,
            nodelay: NodeDelayConfig::normal(),
            connect_timeout: Duration::from_secs(10),
            keep_alive: Some(Duration::from_secs(30)),
            max_retries: 20,
            stream_mode: false,
            socket_buffer_size: None,
            simulate_packet_loss: None,
        }
    }
}

impl NodeDelayConfig {
    /// Normal mode - balanced performance and reliability
    pub fn normal() -> Self {
        Self {
            nodelay: false,
            interval: 40,
            resend: 0,
            no_congestion_control: false,
        }
    }

    /// Fast mode - optimized for low latency
    pub fn fast() -> Self {
        Self {
            nodelay: true,
            interval: 10,  // Reduced from 20ms for lower latency
            resend: 2,
            no_congestion_control: false,
        }
    }

    /// Turbo mode - maximum performance, minimum latency
    pub fn turbo() -> Self {
        Self {
            nodelay: true,
            interval: 5,   // Ultra-low latency, 5ms update interval
            resend: 1,     // Faster resend
            no_congestion_control: true,
        }
    }

    /// Custom configuration
    pub fn custom(nodelay: bool, interval: u32, resend: u32, no_congestion_control: bool) -> Self {
        Self {
            nodelay,
            interval,
            resend,
            no_congestion_control,
        }
    }
}

impl KcpConfig {
    /// Create a new configuration with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Set MTU (Maximum Transmission Unit)
    pub fn mtu(mut self, mtu: u32) -> Self {
        self.mtu = mtu;
        self
    }

    /// Set send window size
    pub fn send_window(mut self, wnd: u32) -> Self {
        self.snd_wnd = wnd;
        self
    }

    /// Set receive window size
    pub fn recv_window(mut self, wnd: u32) -> Self {
        self.rcv_wnd = wnd;
        self
    }

    /// Set both send and receive window sizes
    pub fn window_size(mut self, snd_wnd: u32, rcv_wnd: u32) -> Self {
        self.snd_wnd = snd_wnd;
        self.rcv_wnd = rcv_wnd;
        self
    }

    /// Use normal mode (default)
    pub fn normal_mode(mut self) -> Self {
        self.nodelay = NodeDelayConfig::normal();
        self
    }

    /// Use fast mode for low latency
    pub fn fast_mode(mut self) -> Self {
        self.nodelay = NodeDelayConfig::fast();
        self
    }

    /// Use turbo mode for maximum performance
    pub fn turbo_mode(mut self) -> Self {
        self.nodelay = NodeDelayConfig::turbo();
        self
    }

    /// Set custom node delay configuration
    pub fn nodelay_config(mut self, config: NodeDelayConfig) -> Self {
        self.nodelay = config;
        self
    }

    /// Set connection timeout
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Set keep-alive interval
    pub fn keep_alive(mut self, interval: Option<Duration>) -> Self {
        self.keep_alive = interval;
        self
    }

    /// Set maximum retries
    pub fn max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// Enable stream mode (no message boundaries)
    pub fn stream_mode(mut self, enabled: bool) -> Self {
        self.stream_mode = enabled;
        self
    }

    /// Set UDP socket buffer size
    pub fn socket_buffer_size(mut self, size: usize) -> Self {
        self.socket_buffer_size = Some(size);
        self
    }

    /// Enable packet loss simulation for testing
    pub fn simulate_packet_loss(mut self, loss_rate: f32) -> Self {
        if (0.0..=1.0).contains(&loss_rate) {
            self.simulate_packet_loss = Some(loss_rate);
        }
        self
    }

    /// Validate configuration
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

    /// Create a connection to the specified address
    #[cfg(feature = "tokio")]
    pub async fn connect<A: Into<SocketAddr>>(
        self,
        addr: A,
    ) -> Result<crate::async_kcp::KcpStream> {
        self.validate()?;
        crate::async_kcp::KcpStream::connect(addr.into(), self).await
    }

    /// Bind to the specified address and listen for connections
    #[cfg(feature = "tokio")]
    pub async fn listen<A: Into<SocketAddr>>(
        self,
        addr: A,
    ) -> Result<crate::async_kcp::KcpListener> {
        self.validate()?;
        crate::async_kcp::KcpListener::bind(addr.into(), self).await
    }
}

/// Preset configurations for common use cases
impl KcpConfig {
    /// Configuration optimized for games
    pub fn gaming() -> Self {
        Self::default()
            .turbo_mode()
            .window_size(128, 128)
            .mtu(1200)
            .connect_timeout(Duration::from_secs(5))
            .keep_alive(Some(Duration::from_secs(15)))
    }

    /// Configuration optimized for file transfers
    pub fn file_transfer() -> Self {
        Self::default()
            .normal_mode()
            .window_size(256, 256)
            .mtu(1400)
            .stream_mode(true)
            .connect_timeout(Duration::from_secs(30))
            .keep_alive(Some(Duration::from_secs(60)))
    }

    /// Configuration optimized for real-time communication
    pub fn realtime() -> Self {
        Self::default()
            .fast_mode()
            .window_size(64, 64)
            .mtu(1200)
            .connect_timeout(Duration::from_secs(3))
            .keep_alive(Some(Duration::from_secs(10)))
    }

    /// Configuration for testing with simulated network conditions
    pub fn testing(packet_loss: f32) -> Self {
        Self::default()
            .fast_mode()
            .simulate_packet_loss(packet_loss)
            .connect_timeout(Duration::from_secs(5))
    }
}

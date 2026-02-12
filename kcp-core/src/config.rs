//! Configuration types for the KCP core protocol engine

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
            interval: 8,
            resend: 2,
            no_congestion_control: false,
        }
    }

    /// Turbo mode - maximum performance, minimum latency
    pub fn turbo() -> Self {
        Self {
            nodelay: true,
            interval: 4,
            resend: 1,
            no_congestion_control: true,
        }
    }

    /// Gaming mode - specialized for real-time gaming with ultra-low jitter
    pub fn gaming() -> Self {
        Self {
            nodelay: true,
            interval: 3,
            resend: 1,
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

/// Protocol-only configuration for the KCP engine.
///
/// Contains only the fields the engine reads — no transport or I/O settings.
#[derive(Debug, Clone)]
pub struct KcpCoreConfig {
    /// Maximum transmission unit
    pub mtu: u32,
    /// Send window size
    pub snd_wnd: u32,
    /// Receive window size
    pub rcv_wnd: u32,
    /// Node delay configuration
    pub nodelay: NodeDelayConfig,
    /// Maximum retransmissions before giving up
    pub max_retries: u32,
    /// Enable stream mode (no message boundaries)
    pub stream_mode: bool,
}

impl Default for KcpCoreConfig {
    fn default() -> Self {
        Self {
            mtu: 1400,
            snd_wnd: 32,
            rcv_wnd: 128,
            nodelay: NodeDelayConfig::normal(),
            max_retries: 20,
            stream_mode: false,
        }
    }
}

//! Common types and utilities for KCP implementation

use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// KCP protocol constants
pub mod constants {
    pub const IKCP_RTO_NDL: u32 = 30; // no delay min rto
    pub const IKCP_RTO_MIN: u32 = 100; // normal min rto
    pub const IKCP_RTO_DEF: u32 = 200; // default rto
    pub const IKCP_RTO_MAX: u32 = 60000; // max rto
    pub const IKCP_CMD_PUSH: u8 = 81; // cmd: push data
    pub const IKCP_CMD_ACK: u8 = 82; // cmd: ack
    pub const IKCP_CMD_WASK: u8 = 83; // cmd: window probe (ask)
    pub const IKCP_CMD_WINS: u8 = 84; // cmd: window size (tell)
    pub const IKCP_ASK_SEND: u32 = 1; // need to send IKCP_CMD_WASK
    pub const IKCP_ASK_TELL: u32 = 2; // need to send IKCP_CMD_WINS
    pub const IKCP_WND_SND: u32 = 32; // default send window
    pub const IKCP_WND_RCV: u32 = 128; // default receive window
    pub const IKCP_MTU_DEF: u32 = 1400; // default mtu
    pub const IKCP_ACK_FAST: u32 = 3; // fast ack threshold
    pub const IKCP_INTERVAL: u32 = 100; // default update interval
    pub const IKCP_OVERHEAD: u32 = 24; // kcp header overhead
    pub const IKCP_DEADLINK: u32 = 20; // max dead link count
    pub const IKCP_THRESH_INIT: u32 = 2; // initial slow start threshold
    pub const IKCP_THRESH_MIN: u32 = 2; // min slow start threshold
    pub const IKCP_PROBE_INIT: u32 = 7000; // 7 secs to probe window size
    pub const IKCP_PROBE_LIMIT: u32 = 120000; // up to 120 secs to probe window
    pub const IKCP_FASTACK_LIMIT: u32 = 5; // max times to trigger fastack
}

/// Conversation ID type
pub type ConvId = u32;

/// Sequence number type
pub type SeqNum = u32;

/// Timestamp type (milliseconds since epoch)
pub type Timestamp = u32;

/// KCP segment header structure
#[derive(Debug, Clone, PartialEq)]
pub struct KcpHeader {
    pub conv: ConvId,
    pub cmd: u8,
    pub frg: u8,
    pub wnd: u16,
    pub ts: Timestamp,
    pub sn: SeqNum,
    pub una: SeqNum,
    pub len: u32,
}

impl KcpHeader {
    /// Size of KCP header in bytes
    pub const SIZE: usize = 24;

    /// Create a new header
    pub fn new(conv: ConvId, cmd: u8) -> Self {
        Self {
            conv,
            cmd,
            frg: 0,
            wnd: 0,
            ts: current_timestamp(),
            sn: 0,
            una: 0,
            len: 0,
        }
    }

    /// Encode header into buffer
    pub fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32_le(self.conv);
        buf.put_u8(self.cmd);
        buf.put_u8(self.frg);
        buf.put_u16_le(self.wnd);
        buf.put_u32_le(self.ts);
        buf.put_u32_le(self.sn);
        buf.put_u32_le(self.una);
        buf.put_u32_le(self.len);
    }

    /// Decode header from buffer
    pub fn decode(buf: &mut Bytes) -> Option<Self> {
        if buf.len() < Self::SIZE {
            return None;
        }

        Some(Self {
            conv: buf.get_u32_le(),
            cmd: buf.get_u8(),
            frg: buf.get_u8(),
            wnd: buf.get_u16_le(),
            ts: buf.get_u32_le(),
            sn: buf.get_u32_le(),
            una: buf.get_u32_le(),
            len: buf.get_u32_le(),
        })
    }

    /// Get command type as string for debugging
    pub fn cmd_str(&self) -> &'static str {
        match self.cmd {
            constants::IKCP_CMD_PUSH => "PUSH",
            constants::IKCP_CMD_ACK => "ACK",
            constants::IKCP_CMD_WASK => "WASK",
            constants::IKCP_CMD_WINS => "WINS",
            _ => "UNKNOWN",
        }
    }
}

/// KCP segment containing header and data
#[derive(Debug, Clone)]
pub struct KcpSegment {
    pub header: KcpHeader,
    pub data: Bytes,

    // Internal fields for protocol logic
    pub resendts: Timestamp,
    pub rto: u32,
    pub fastack: u32,
    pub xmit: u32,
}

impl KcpSegment {
    /// Create a new segment
    pub fn new(conv: ConvId, cmd: u8, data: Bytes) -> Self {
        let mut header = KcpHeader::new(conv, cmd);
        header.len = data.len() as u32;

        Self {
            header,
            data,
            resendts: 0,
            rto: constants::IKCP_RTO_DEF,
            fastack: 0,
            xmit: 0,
        }
    }

    /// Create PUSH segment
    pub fn push(conv: ConvId, sn: SeqNum, data: Bytes) -> Self {
        let mut seg = Self::new(conv, constants::IKCP_CMD_PUSH, data);
        seg.header.sn = sn;
        seg
    }

    /// Create ACK segment
    pub fn ack(conv: ConvId, sn: SeqNum, ts: Timestamp) -> Self {
        let mut seg = Self::new(conv, constants::IKCP_CMD_ACK, Bytes::new());
        seg.header.sn = sn;
        seg.header.ts = ts;
        seg
    }

    /// Create batched ACK segment (for performance optimization)
    /// Note: This is a simplified batching - in practice, would need protocol extension
    pub fn ack_batch(conv: ConvId, acks: Vec<(SeqNum, Timestamp)>) -> Self {
        // For now, just use the first ACK as primary (protocol compatibility)
        // In a real implementation, this would require protocol extensions
        if let Some((sn, ts)) = acks.first() {
            Self::ack(conv, *sn, *ts)
        } else {
            Self::ack(conv, 0, 0)
        }
    }

    /// Encode segment into buffer
    pub fn encode(&self, buf: &mut BytesMut) {
        self.header.encode(buf);
        buf.extend_from_slice(&self.data);
    }

    /// Decode segment from buffer
    pub fn decode(mut buf: Bytes) -> Option<Self> {
        let header = KcpHeader::decode(&mut buf)?;

        if buf.len() != header.len as usize {
            return None;
        }

        Some(Self {
            header,
            data: buf,
            resendts: 0,
            rto: constants::IKCP_RTO_DEF,
            fastack: 0,
            xmit: 0,
        })
    }

    /// Get total segment size
    pub fn size(&self) -> usize {
        KcpHeader::SIZE + self.data.len()
    }

    /// Check if this is a data segment
    pub fn is_data(&self) -> bool {
        self.header.cmd == constants::IKCP_CMD_PUSH
    }

    /// Check if this is an ACK segment
    pub fn is_ack(&self) -> bool {
        self.header.cmd == constants::IKCP_CMD_ACK
    }

    /// Check if this is a probe segment
    pub fn is_probe(&self) -> bool {
        matches!(
            self.header.cmd,
            constants::IKCP_CMD_WASK | constants::IKCP_CMD_WINS
        )
    }
}

/// Statistics for KCP connection
#[derive(Debug, Default, Clone)]
pub struct KcpStats {
    /// Total bytes sent
    pub bytes_sent: u64,
    /// Total bytes received
    pub bytes_received: u64,
    /// Total packets sent
    pub packets_sent: u64,
    /// Total packets received
    pub packets_received: u64,
    /// Total retransmissions
    pub retransmissions: u64,
    /// Fast retransmissions
    pub fast_retransmissions: u64,
    /// Current RTT in milliseconds
    pub rtt: u32,
    /// RTT variance
    pub rtt_var: u32,
    /// Current RTO
    pub rto: u32,
    /// Send window size
    pub snd_wnd: u32,
    /// Receive window size
    pub rcv_wnd: u32,
    /// Congestion window size
    pub cwnd: u32,
    /// Packets in send buffer
    pub snd_buf_size: u32,
    /// Packets in receive buffer
    pub rcv_buf_size: u32,
}

/// Get current timestamp in milliseconds
pub fn current_timestamp() -> Timestamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as Timestamp
}

/// Calculate time difference handling wrapping
pub fn time_diff(later: Timestamp, earlier: Timestamp) -> i32 {
    later.wrapping_sub(earlier) as i32
}

/// Check if a sequence number is before another (handling wrapping)
pub fn seq_before(seq1: SeqNum, seq2: SeqNum) -> bool {
    (seq1.wrapping_sub(seq2) as i32) < 0
}

/// Check if a sequence number is after another (handling wrapping)
pub fn seq_after(seq1: SeqNum, seq2: SeqNum) -> bool {
    (seq1.wrapping_sub(seq2) as i32) > 0
}

/// High-performance lock-free buffer pool using crossbeam-queue
pub struct BufferPool {
    pool: crossbeam_queue::ArrayQueue<BytesMut>,
    buffer_size: usize,
    hits: std::sync::atomic::AtomicUsize,
}

impl BufferPool {
    /// Create a new buffer pool
    pub fn new(max_size: usize, buffer_size: usize) -> Self {
        Self {
            pool: crossbeam_queue::ArrayQueue::new(max_size),
            buffer_size,
            hits: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Get a buffer from the pool (lock-free)
    pub fn try_get(&self) -> BytesMut {
        match self.pool.pop() {
            Some(buf) => {
                self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                buf
            }
            None => BytesMut::with_capacity(self.buffer_size),
        }
    }

    /// Return a buffer to the pool (lock-free)
    pub fn try_put(&self, mut buf: BytesMut) {
        // Only return buffers of appropriate size
        if buf.capacity() >= self.buffer_size / 2 && buf.capacity() <= self.buffer_size * 2 {
            buf.clear();
            let _ = self.pool.push(buf); // Ignore if full
        }
    }

    /// Get pool statistics (hits, current_size)
    pub fn stats(&self) -> (usize, usize) {
        (
            self.hits.load(std::sync::atomic::Ordering::Relaxed),
            self.pool.len(),
        )
    }
}

static SMALL_BUFFER_POOL: LazyLock<BufferPool> = LazyLock::new(|| BufferPool::new(4000, 1024));
static MEDIUM_BUFFER_POOL: LazyLock<BufferPool> = LazyLock::new(|| BufferPool::new(2000, 1400));
static LARGE_BUFFER_POOL: LazyLock<BufferPool> = LazyLock::new(|| BufferPool::new(1000, 8192));
static JUMBO_BUFFER_POOL: LazyLock<BufferPool> = LazyLock::new(|| BufferPool::new(200, 65536));

/// Get a buffer from the global pool (non-blocking)
pub fn try_get_buffer(size_hint: usize) -> BytesMut {
    if size_hint <= 1024 {
        SMALL_BUFFER_POOL.try_get()
    } else if size_hint <= 1400 {
        MEDIUM_BUFFER_POOL.try_get()
    } else if size_hint <= 8192 {
        LARGE_BUFFER_POOL.try_get()
    } else {
        JUMBO_BUFFER_POOL.try_get()
    }
}

/// Return a buffer to the global pool (non-blocking)
pub fn try_put_buffer(buf: BytesMut) {
    let capacity = buf.capacity();
    if capacity <= 1536 {
        SMALL_BUFFER_POOL.try_put(buf);
    } else if capacity <= 2100 {
        MEDIUM_BUFFER_POOL.try_put(buf);
    } else if capacity <= 12288 {
        LARGE_BUFFER_POOL.try_put(buf);
    } else {
        JUMBO_BUFFER_POOL.try_put(buf);
    }
}

/// Get buffer pool statistics for monitoring
pub fn buffer_pool_stats() -> Vec<(&'static str, usize, usize)> {
    vec![
        (
            "small",
            SMALL_BUFFER_POOL.stats().0,
            SMALL_BUFFER_POOL.stats().1,
        ),
        (
            "medium",
            MEDIUM_BUFFER_POOL.stats().0,
            MEDIUM_BUFFER_POOL.stats().1,
        ),
        (
            "large",
            LARGE_BUFFER_POOL.stats().0,
            LARGE_BUFFER_POOL.stats().1,
        ),
        (
            "jumbo",
            JUMBO_BUFFER_POOL.stats().0,
            JUMBO_BUFFER_POOL.stats().1,
        ),
    ]
}

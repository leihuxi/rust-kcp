//! Async KCP protocol engine core

use crate::common::*;
use crate::config::KcpConfig;
use crate::error::{ConnectionError, KcpError, Result};

use bytes::{Bytes, BytesMut};
use std::collections::{BTreeMap, VecDeque};
use tracing::{info, trace, warn};

/// RTT calculation state
#[derive(Debug, Default)]
struct RttState {
    avg: u32,     // Smoothed RTT
    var: u32,     // RTT variance
    rto: u32,     // Retransmission timeout
    min_rto: u32, // Minimum RTO
}

/// Window control state
#[derive(Debug)]
struct WindowState {
    snd: u32,      // Send window size
    rcv: u32,      // Receive window size
    rmt: u32,      // Remote window size
    cwnd: u32,     // Congestion window
    ssthresh: u32, // Slow start threshold
    incr: u32,     // Increment for congestion avoidance
}

/// Probe state for window probing
#[derive(Debug, Default)]
struct ProbeState {
    flags: u32,
    wait: u32,
    ts: Timestamp,
}

/// Async KCP engine implementing the core protocol logic
///
/// All methods are synchronous. Output packets are buffered in `output_queue`
/// and must be drained by the caller via [`drain_output`](Self::drain_output).
pub struct KcpEngine {
    // Core
    conv: ConvId,
    config: KcpConfig,

    // Sequence numbers
    snd_una: SeqNum,
    snd_nxt: SeqNum,
    rcv_nxt: SeqNum,

    // Timing and window
    rtt: RttState,
    wnd: WindowState,
    probe: ProbeState,

    // Buffers
    snd_queue: VecDeque<KcpSegment>,
    rcv_queue: VecDeque<KcpSegment>,
    snd_buf: VecDeque<KcpSegment>,
    rcv_buf: BTreeMap<SeqNum, KcpSegment>,
    ack_list: Vec<(SeqNum, Timestamp)>,

    // Output queue (replaces OutputFn)
    output_queue: Vec<Bytes>,

    // State
    stats: KcpStats,
    last_update: Timestamp,
    last_activity: Timestamp,
    dead_link: u32,
    xmit_count: u32,
}

impl KcpEngine {
    /// Create a new KCP engine
    pub fn new(conv: ConvId, config: KcpConfig) -> Self {
        let min_rto = if config.nodelay.nodelay {
            constants::IKCP_RTO_NDL
        } else {
            constants::IKCP_RTO_MIN
        };

        Self {
            conv,
            snd_una: 0,
            snd_nxt: 0,
            rcv_nxt: 0,

            rtt: RttState {
                avg: 0,
                var: 0,
                rto: constants::IKCP_RTO_DEF,
                min_rto,
            },

            wnd: WindowState {
                snd: config.snd_wnd,
                rcv: config.rcv_wnd,
                rmt: constants::IKCP_WND_RCV,
                cwnd: config.snd_wnd,
                ssthresh: constants::IKCP_THRESH_INIT,
                incr: 0,
            },

            probe: ProbeState::default(),

            snd_queue: VecDeque::with_capacity(config.snd_wnd as usize),
            rcv_queue: VecDeque::with_capacity(config.rcv_wnd as usize),
            snd_buf: VecDeque::with_capacity(config.snd_wnd as usize),
            rcv_buf: BTreeMap::new(),
            ack_list: Vec::with_capacity(config.rcv_wnd as usize),

            output_queue: Vec::with_capacity(config.snd_wnd as usize),

            stats: KcpStats::default(),
            last_update: current_timestamp(),
            last_activity: current_timestamp(),
            dead_link: config.max_retries,
            xmit_count: 0,

            config,
        }
    }

    /// Start the engine
    pub fn start(&mut self) -> Result<()> {
        info!(conv = %self.conv, "KCP engine started");
        Ok(())
    }

    /// Drain buffered output packets. The caller is responsible for sending
    /// each `Bytes` over the transport.
    pub fn drain_output(&mut self) -> Vec<Bytes> {
        std::mem::take(&mut self.output_queue)
    }

    /// Send data through KCP
    pub fn send(&mut self, data: Bytes) -> Result<()> {
        self.last_activity = current_timestamp();
        if data.is_empty() {
            return Ok(());
        }

        let mss = self.mss() as usize;
        let mut data = data;

        // Stream mode: try to merge into the last queued segment
        if self.config.stream_mode {
            if let Some(last) = self.snd_queue.back_mut() {
                let old_len = last.data.len();
                if old_len < mss {
                    let capacity = mss - old_len;
                    let extend = std::cmp::min(data.len(), capacity);
                    let mut buf = BytesMut::with_capacity(old_len + extend);
                    buf.extend_from_slice(&last.data);
                    buf.extend_from_slice(&data[..extend]);
                    last.data = buf.freeze();
                    last.header.len = last.data.len() as u32;
                    data = data.slice(extend..);
                    if data.is_empty() {
                        self.stats.bytes_sent += extend as u64;
                        self.flush()?;
                        return Ok(());
                    }
                }
            }
        }

        let count = if data.len() <= mss {
            1
        } else {
            data.len().div_ceil(mss)
        };

        // Check if we can send all fragments
        if count >= constants::IKCP_WND_RCV as usize {
            return Err(KcpError::buffer("Message too large for window"));
        }

        // Fragment the data
        let mut offset = 0;
        for i in 0..count {
            let size = std::cmp::min(mss, data.len() - offset);
            let fragment = data.slice(offset..offset + size);

            let mut segment = KcpSegment::push(self.conv, 0, fragment);

            // Set fragment number (remaining fragments) — only in message mode
            if !self.config.stream_mode {
                segment.header.frg = (count - i - 1) as u8;
            }

            self.snd_queue.push_back(segment);
            offset += size;
        }

        self.stats.bytes_sent += data.len() as u64;

        // Trigger immediate flush if possible
        self.flush()?;

        trace!(
            conv = %self.conv,
            bytes = data.len(),
            fragments = count,
            "Data queued for sending"
        );

        Ok(())
    }

    /// Receive data from KCP
    pub fn recv(&mut self) -> Result<Option<Bytes>> {
        if self.rcv_queue.is_empty() {
            return Ok(None);
        }

        // Check if we have a complete message
        let peek_size = self.peek_size();
        if peek_size <= 0 {
            return Ok(None);
        }

        // Assemble the message from fragments using optimized buffer pool
        let mut data = crate::common::try_get_buffer(peek_size as usize);
        let mut recovered = false;

        // Check if we were window-limited before
        if self.rcv_queue.len() >= self.wnd.rcv as usize {
            recovered = true;
        }

        while let Some(segment) = self.rcv_queue.front() {
            if data.len() + segment.data.len() > peek_size as usize {
                break;
            }

            let segment = self.rcv_queue.pop_front().unwrap();
            data.extend_from_slice(&segment.data);

            if segment.header.frg == 0 {
                break; // Last fragment
            }
        }

        if data.is_empty() {
            return Ok(None);
        }

        self.stats.bytes_received += data.len() as u64;

        // Move segments from receive buffer to receive queue
        self.move_to_recv_queue();

        // Trigger window update if we recovered space
        if recovered && self.rcv_queue.len() < self.wnd.rcv as usize {
            self.probe.flags |= constants::IKCP_ASK_TELL;
        }

        trace!(
            conv = %self.conv,
            bytes = data.len(),
            "Data received"
        );

        // Freeze the data and return it
        let result = data.freeze();
        Ok(Some(result))
    }

    /// Process incoming packet
    pub fn input(&mut self, data: Bytes) -> Result<()> {
        let current = current_timestamp();
        self.last_activity = current;
        if data.len() < KcpHeader::SIZE {
            return Err(KcpError::protocol("Packet too small"));
        }

        let original_size = data.len();
        let mut buf = data;
        let mut flag = false;
        let mut max_ack = 0;
        let mut latest_ts = 0;

        // Process all segments in the packet
        while buf.len() >= KcpHeader::SIZE {
            let segment = match KcpSegment::decode(buf.clone()) {
                Some(seg) => {
                    buf = buf.slice(seg.size()..);
                    seg
                }
                None => break,
            };

            // Verify conversation ID — skip this segment but keep processing the rest
            if segment.header.conv != self.conv {
                warn!(
                    conv = %self.conv,
                    packet_conv = %segment.header.conv,
                    "Conversation ID mismatch, skipping segment"
                );
                continue;
            }

            // Update remote window
            self.wnd.rmt = segment.header.wnd as u32;

            // Process UNA (unacknowledged sequence number)
            self.parse_una(segment.header.una);
            self.shrink_buf();

            match segment.header.cmd {
                constants::IKCP_CMD_ACK => {
                    // Process ACK
                    if current >= segment.header.ts {
                        let rtt = current - segment.header.ts;
                        self.update_ack(rtt as i32);
                    }

                    self.parse_ack(segment.header.sn);
                    self.shrink_buf();

                    if !flag {
                        flag = true;
                        max_ack = segment.header.sn;
                        latest_ts = segment.header.ts;
                    } else if seq_after(segment.header.sn, max_ack) {
                        max_ack = segment.header.sn;
                        latest_ts = segment.header.ts;
                    }
                }

                constants::IKCP_CMD_PUSH => {
                    // Process data segment
                    if seq_before(segment.header.sn, self.rcv_nxt + self.wnd.rcv) {
                        // Add ACK
                        self.ack_push(segment.header.sn, segment.header.ts);

                        if !seq_before(segment.header.sn, self.rcv_nxt) {
                            self.parse_data(segment);
                        }
                    }
                }

                constants::IKCP_CMD_WASK => {
                    // Window probe request
                    self.probe.flags |= constants::IKCP_ASK_TELL;
                }

                constants::IKCP_CMD_WINS => {
                    // Window probe response - no action needed
                }

                _ => {
                    warn!(
                        conv = %self.conv,
                        cmd = segment.header.cmd,
                        "Unknown command"
                    );
                }
            }
        }

        // Process fast ACK
        if flag {
            self.parse_fastack(max_ack, latest_ts);
        }

        // Update congestion window
        self.update_cwnd();

        self.stats.packets_received += 1;

        trace!(
            conv = %self.conv,
            size = original_size,
            "Packet processed"
        );

        Ok(())
    }

    /// Flush pending data and ACKs
    pub fn flush(&mut self) -> Result<()> {
        let current = current_timestamp();

        // Flush ACKs
        self.flush_acks(current)?;

        // Handle window probing
        self.handle_window_probe(current)?;

        // Move data from send queue to send buffer
        self.move_to_send_buf(current);

        // Flush data segments
        self.flush_data_segments(current)?;

        // Update congestion control
        self.update_congestion_control();

        Ok(())
    }

    /// Update KCP state (called periodically)
    pub fn update(&mut self) -> Result<()> {
        let current = current_timestamp();

        if current < self.last_update {
            self.last_update = current;
        }

        let diff = current - self.last_update;
        if diff >= self.config.nodelay.interval {
            self.last_update = current;
            self.flush()?;
        }

        Ok(())
    }

    /// Get current statistics
    pub fn stats(&self) -> &KcpStats {
        &self.stats
    }

    /// Check if connection is alive
    pub fn is_dead(&self) -> bool {
        self.xmit_count >= self.dead_link
    }

    /// Milliseconds since last send or receive activity
    pub fn idle_ms(&self) -> u32 {
        let now = current_timestamp();
        now.saturating_sub(self.last_activity)
    }

    /// Trigger a window probe to keep the connection alive
    pub fn keep_alive_probe(&mut self) -> Result<()> {
        self.probe.flags |= constants::IKCP_ASK_SEND;
        self.flush()
    }

    // Private helper methods

    fn peek_size(&self) -> i32 {
        if self.rcv_queue.is_empty() {
            return -1;
        }

        let seg = &self.rcv_queue[0];
        if seg.header.frg == 0 {
            return seg.data.len() as i32;
        }

        if self.rcv_queue.len() < (seg.header.frg + 1) as usize {
            return -1;
        }

        let mut length = 0;
        for segment in &self.rcv_queue {
            length += segment.data.len();
            if segment.header.frg == 0 {
                break;
            }
        }

        length as i32
    }

    fn parse_una(&mut self, una: SeqNum) {
        while let Some(segment) = self.snd_buf.front() {
            if seq_before(segment.header.sn, una) {
                self.snd_buf.pop_front();
            } else {
                break;
            }
        }
    }

    fn parse_ack(&mut self, sn: SeqNum) {
        if seq_before(sn, self.snd_una) || !seq_before(sn, self.snd_nxt) {
            return;
        }

        self.snd_buf.retain(|seg| seg.header.sn != sn);
    }

    fn parse_fastack(&mut self, sn: SeqNum, _ts: Timestamp) {
        if seq_before(sn, self.snd_una) || !seq_before(sn, self.snd_nxt) {
            return;
        }

        for segment in &mut self.snd_buf {
            if seq_before(segment.header.sn, sn) {
                segment.fastack += 1;
            } else if segment.header.sn != sn {
                break;
            }
        }
    }

    fn parse_data(&mut self, newseg: KcpSegment) {
        let sn = newseg.header.sn;

        if !seq_before(sn, self.rcv_nxt + self.wnd.rcv) || seq_before(sn, self.rcv_nxt) {
            return;
        }

        // O(log n) insert with automatic dedup
        self.rcv_buf.entry(sn).or_insert(newseg);

        // Move consecutive segments to receive queue
        self.move_to_recv_queue();
    }

    fn move_to_recv_queue(&mut self) {
        while let Some(segment) = self.rcv_buf.remove(&self.rcv_nxt) {
            if self.rcv_queue.len() >= self.wnd.rcv as usize {
                self.rcv_buf.insert(self.rcv_nxt, segment);
                break;
            }
            self.rcv_queue.push_back(segment);
            self.rcv_nxt += 1;
        }
    }

    fn ack_push(&mut self, sn: SeqNum, ts: Timestamp) {
        self.ack_list.push((sn, ts));
    }

    fn update_ack(&mut self, rtt: i32) {
        if self.rtt.avg == 0 {
            self.rtt.avg = rtt as u32;
            self.rtt.var = rtt as u32 / 2;
        } else {
            let delta = if rtt > self.rtt.avg as i32 {
                rtt - self.rtt.avg as i32
            } else {
                self.rtt.avg as i32 - rtt
            };

            self.rtt.var = (3 * self.rtt.var + delta as u32) / 4;
            self.rtt.avg = (7 * self.rtt.avg + rtt as u32) / 8;

            if self.rtt.avg < 1 {
                self.rtt.avg = 1;
            }
        }

        let rto = self.rtt.avg + 4 * self.rtt.var.max(self.config.nodelay.interval);
        self.rtt.rto = rto.clamp(self.rtt.min_rto, constants::IKCP_RTO_MAX);

        self.stats.rtt = self.rtt.avg;
        self.stats.rtt_var = self.rtt.var;
        self.stats.rto = self.rtt.rto;
    }

    fn shrink_buf(&mut self) {
        if let Some(segment) = self.snd_buf.front() {
            self.snd_una = segment.header.sn;
        } else {
            self.snd_una = self.snd_nxt;
        }
    }

    fn flush_acks(&mut self, _current: Timestamp) -> Result<()> {
        let ack_count = self.ack_list.len();
        if ack_count == 0 {
            return Ok(());
        }

        let wnd = self.wnd_unused() as u16;
        let una = self.rcv_nxt;

        let mut segments = Vec::with_capacity(ack_count);

        for (sn, ts) in self.ack_list.drain(..) {
            let mut seg = KcpSegment::ack(self.conv, sn, ts);
            seg.header.wnd = wnd;
            seg.header.una = una;
            segments.push(seg);
        }

        for segment in segments {
            self.output_segment(segment)?;
        }

        Ok(())
    }

    fn handle_window_probe(&mut self, current: Timestamp) -> Result<()> {
        if self.wnd.rmt == 0 {
            if self.probe.wait == 0 {
                self.probe.wait = constants::IKCP_PROBE_INIT;
                self.probe.ts = current + self.probe.wait;
            } else if time_diff(current, self.probe.ts) >= 0 {
                if self.probe.wait < constants::IKCP_PROBE_INIT {
                    self.probe.wait = constants::IKCP_PROBE_INIT;
                }
                self.probe.wait += self.probe.wait / 2;
                if self.probe.wait > constants::IKCP_PROBE_LIMIT {
                    self.probe.wait = constants::IKCP_PROBE_LIMIT;
                }
                self.probe.ts = current + self.probe.wait;
                self.probe.flags |= constants::IKCP_ASK_SEND;
            }
        } else {
            self.probe.ts = 0;
            self.probe.wait = 0;
        }

        // Send probe packets
        if (self.probe.flags & constants::IKCP_ASK_SEND) != 0 {
            let segment = self.create_probe_segment(constants::IKCP_CMD_WASK);
            self.output_segment(segment)?;
        }

        if (self.probe.flags & constants::IKCP_ASK_TELL) != 0 {
            let segment = self.create_probe_segment(constants::IKCP_CMD_WINS);
            self.output_segment(segment)?;
        }

        self.probe.flags = 0;
        Ok(())
    }

    fn move_to_send_buf(&mut self, current: Timestamp) {
        let cwnd = std::cmp::min(self.wnd.snd, self.wnd.rmt);
        let cwnd = if self.config.nodelay.no_congestion_control {
            cwnd
        } else {
            std::cmp::min(self.wnd.cwnd, cwnd)
        };

        while time_diff(self.snd_nxt, self.snd_una + cwnd) < 0 {
            if let Some(mut segment) = self.snd_queue.pop_front() {
                segment.header.conv = self.conv;
                segment.header.cmd = constants::IKCP_CMD_PUSH;
                segment.header.wnd = self.wnd_unused() as u16;
                segment.header.ts = current;
                segment.header.sn = self.snd_nxt;
                segment.header.una = self.rcv_nxt;
                segment.resendts = current;
                segment.rto = self.rtt.rto;
                segment.fastack = 0;
                segment.xmit = 0;

                self.snd_buf.push_back(segment);
                self.snd_nxt += 1;
            } else {
                break;
            }
        }
    }

    fn flush_data_segments(&mut self, current: Timestamp) -> Result<()> {
        let resend = if self.config.nodelay.resend > 0 {
            self.config.nodelay.resend
        } else {
            u32::MAX
        };

        let rtomin = if self.config.nodelay.nodelay {
            0
        } else {
            self.rtt.rto / 8
        };

        let mut lost = false;
        let mut change = false;

        let mut send_indices: Vec<usize> = Vec::new();
        let wnd_unused = self.wnd_unused() as u16;
        let rcv_nxt = self.rcv_nxt;

        for (i, segment) in self.snd_buf.iter_mut().enumerate() {
            let mut needsend = false;

            if segment.xmit == 0 {
                // First transmission
                needsend = true;
                segment.xmit = 1;
                segment.rto = self.rtt.rto;
                segment.resendts = current + segment.rto + rtomin;
            } else if time_diff(current, segment.resendts) >= 0 {
                // Timeout retransmission
                needsend = true;
                segment.xmit += 1;
                self.xmit_count += 1;
                self.stats.retransmissions += 1;

                if self.config.nodelay.nodelay {
                    let step = if self.config.nodelay.no_congestion_control {
                        self.rtt.rto
                    } else {
                        segment.rto
                    };
                    segment.rto += step / 2;
                } else {
                    segment.rto += std::cmp::max(segment.rto, self.config.nodelay.interval);
                }

                segment.resendts = current + segment.rto;
                lost = true;
            } else if segment.fastack >= resend {
                // Fast retransmission
                if segment.xmit <= constants::IKCP_FASTACK_LIMIT
                    || constants::IKCP_FASTACK_LIMIT == 0
                {
                    needsend = true;
                    segment.xmit += 1;
                    segment.fastack = 0;
                    segment.resendts = current + segment.rto;
                    self.stats.fast_retransmissions += 1;
                    change = true;
                }
            }

            if needsend {
                segment.header.ts = current;
                segment.header.wnd = wnd_unused;
                segment.header.una = rcv_nxt;

                send_indices.push(i);
            }
        }

        for i in send_indices {
            let segment = &self.snd_buf[i];
            let is_dead = segment.xmit >= self.dead_link;
            let mut buf = crate::common::try_get_buffer(segment.size());
            segment.encode(&mut buf);
            self.output_queue.push(buf.freeze());
            self.stats.packets_sent += 1;

            if is_dead {
                return Err(KcpError::connection(ConnectionError::Lost));
            }
        }

        // Update congestion control state
        if change {
            let inflight = self.snd_nxt - self.snd_una;
            self.wnd.ssthresh = inflight / 2;
            if self.wnd.ssthresh < constants::IKCP_THRESH_MIN {
                self.wnd.ssthresh = constants::IKCP_THRESH_MIN;
            }
            self.wnd.cwnd = self.wnd.ssthresh + resend;
            self.wnd.incr = self.wnd.cwnd * self.mss();
        }

        if lost {
            self.wnd.ssthresh = std::cmp::max(self.wnd.cwnd / 2, constants::IKCP_THRESH_MIN);
            self.reset_cwnd();
        }

        if self.wnd.cwnd < 1 {
            self.reset_cwnd();
        }

        Ok(())
    }

    fn update_cwnd(&mut self) {
        if time_diff(self.snd_una, self.wnd.cwnd) > 0 && self.wnd.cwnd < self.wnd.rmt {
            let mss = self.mss();
            if self.wnd.cwnd < self.wnd.ssthresh {
                self.wnd.cwnd += 1;
                self.wnd.incr += mss;
            } else {
                if self.wnd.incr < mss {
                    self.wnd.incr = mss;
                }
                self.wnd.incr += (mss * mss) / self.wnd.incr + (mss / 16);
                if (self.wnd.cwnd + 1) * mss <= self.wnd.incr {
                    self.wnd.cwnd = if mss > 0 {
                        self.wnd.incr.div_ceil(mss)
                    } else {
                        1
                    };
                }
            }
            if self.wnd.cwnd > self.wnd.rmt {
                self.wnd.cwnd = self.wnd.rmt;
                self.wnd.incr = self.wnd.rmt * mss;
            }
        }
    }

    fn update_congestion_control(&mut self) {
        self.stats.snd_wnd = self.wnd.snd;
        self.stats.rcv_wnd = self.wnd.rcv;
        self.stats.cwnd = self.wnd.cwnd;
        self.stats.snd_buf_size = self.snd_buf.len() as u32;
        self.stats.rcv_buf_size = self.rcv_buf.len() as u32;
    }

    fn output_segment(&mut self, segment: KcpSegment) -> Result<()> {
        let mut buf = crate::common::try_get_buffer(segment.size());
        segment.encode(&mut buf);
        self.output_queue.push(buf.freeze());
        self.stats.packets_sent += 1;
        Ok(())
    }

    fn wnd_unused(&self) -> u32 {
        if self.rcv_queue.len() < self.wnd.rcv as usize {
            self.wnd.rcv - self.rcv_queue.len() as u32
        } else {
            0
        }
    }

    /// Maximum segment size (MTU - overhead)
    #[inline]
    fn mss(&self) -> u32 {
        self.config.mtu - constants::IKCP_OVERHEAD
    }

    /// Reset congestion window to initial state
    #[inline]
    fn reset_cwnd(&mut self) {
        self.wnd.cwnd = 1;
        self.wnd.incr = self.mss();
    }

    /// Create a probe segment with common fields set
    fn create_probe_segment(&self, cmd: u8) -> KcpSegment {
        let mut segment = KcpSegment::new(self.conv, cmd, Bytes::new());
        segment.header.wnd = self.wnd_unused() as u16;
        segment.header.una = self.rcv_nxt;
        segment
    }
}

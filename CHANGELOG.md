# Changelog

All notable changes to this project are documented here.

## [kcp-tokio 0.6.0] / [kcp-core 0.2.0] — 2026-06-05

A correctness, robustness, and architecture release. No wire-format changes —
interoperable with previous versions and the canonical C KCP / `tokio_kcp`.

### Fixed (reliability & robustness)

- **Receive-side data loss under backpressure**: the actor no longer drops an
  already-ACKed message when the application read channel is full. Capacity is
  reserved before a message leaves the engine, so undeliverable data stays
  queued (and keeps the receive window honest) instead of being lost.
- **`is_dead()` false positives**: replaced a cumulative retransmission counter
  that never reset (so any long-lived connection eventually reported dead) with
  the canonical per-segment dead-link check.
- **Congestion-window growth gating**: `update_cwnd` now grows the window only
  when an ACK actually advances `snd_una` (matching canonical KCP), instead of
  comparing a sequence number against the window size.
- **Remote panic on malformed `frg`**: a peer-controlled fragment count of 255
  no longer overflows in `peek_size` (panicked in debug, wrapped in release).
- **RTT estimator hardening**: RTT samples use wrapping time arithmetic and are
  clamped, so a bogus echoed timestamp can't skip samples or overflow the
  smoothing math.
- **Keep-alive probe storm**: probes are throttled to one per keep-alive window
  instead of firing every update tick.
- **Listener input validation**: datagrams whose command byte isn't a valid KCP
  command no longer open phantom pending connections.
- **Window size validation**: `KcpConfig::validate` rejects windows above the
  u16 wire field (65535) instead of silently truncating advertised windows.
- **Error predicate**: `ConnectionError::Lost` is no longer reported as both
  recoverable and fatal.

### Added

- **Send-side flow control**: writes flow through a bounded channel with an
  engine high-water mark; `poll_write` / `KcpStream::send` apply real
  backpressure instead of growing the send queue without bound (OOM on a fast
  writer over a slow link). Outbound stream writes are chunked to one MSS, and
  `send()` fails fast on an oversized single message.
- **Graceful close drain**: on close the actor keeps flushing and
  retransmitting until the peer has acknowledged all in-flight data (or a linger
  deadline passes), so the tail of a stream is no longer dropped.
- **Event-driven scheduling**: `KcpEngine::check()` reports the next deadline and
  the driver sleeps until then instead of waking every fixed interval — idle
  connections cost almost nothing, and retransmits fire at RTO precision.
- **`KcpConfig::simulate_packet_loss`** is now wired into the send path (was a
  silent no-op), so the `testing()` preset actually exercises retransmission.
- New `KcpEngine` methods: `check`, `send_queue_len`, `has_unsent_data`.

### Changed

- **Monotonic clock**: KCP timestamps are now derived from a monotonic source
  rather than the wall clock, so NTP steps / clock changes no longer corrupt
  RTT/RTO mid-connection.

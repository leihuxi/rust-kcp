# Changelog

All notable changes to this project are documented here.

## [kcp-tokio 0.7.0] / [kcp-core 0.3.0] — 2026-06-10

A correctness pass over close semantics, liveness, and configuration safety,
plus a performance pass over the send path. No wire-format changes.

### Changed (kcp-core semantics)

- **`KcpEngine::send()` no longer flushes internally** — it only queues, like
  canonical KCP's `ikcp_send`; the caller drives emission via `flush()` /
  `update()`. The async actor still flushes in the same event-loop turn, so
  stream latency is unchanged, but messages queued together now share
  MTU-packed datagrams instead of paying one datagram each.

### Performance

Criterion (engine benches, same machine, before → after):
`64B × 1000` messages +26% throughput, `1KB × 500` +20%, large single
messages unchanged.

- **Datagram batching**: the actor absorbs all writes already queued in the
  channel before flushing once; consecutive small messages share datagrams.
- **`poll_write` fast path**: hands each MSS chunk off with `try_send` —
  the per-chunk boxed future is now only allocated under real backpressure.
- **Stream-mode merge is O(n) total**: appends in place via
  `Bytes::try_into_mut` into an mss-capacity buffer instead of re-copying
  the accumulated segment on every small write (was O(n²)).
- **Outbound buffer recycling**: datagrams are carved off `out_buf` with
  `split()`/`reserve()` instead of allocating a fresh `BytesMut` each time.
- **`parse_ack` binary search**: snd_buf is sn-ordered; the linear scan is
  replaced with a wrapping-safe binary search.

### Removed

- **`buffer_pool` module**: it was decorative — the receive path never used
  it and `Bytes::freeze` ownership prevents round-tripping; the one real
  call site now uses a plain `BytesMut`. Drops the `crossbeam-queue`
  dependency.
- **`metrics` module**: the library never updated the global metrics (only
  the perf examples did); per-stream `stats()` remains the supported API.
- `bytes` minimum version is now 1.7 (for `Bytes::try_into_mut`).

### Fixed (reliability & robustness)

- **Tail loss on `shutdown()` + drop**: `poll_shutdown` now waits for the
  actor's graceful drain (peer ACKs all in-flight data, or the linger deadline
  passes) instead of returning immediately, and `Drop` no longer aborts the
  actor mid-drain. `AsyncWriteExt::shutdown().await` followed by drop is now
  loss-free; the client `recv_task` exits on its own once the actor is gone.
- **Server-side connection leak for silently-vanished peers**: keep-alive
  probes are not retransmitted and never counted toward `dead_link`, so an
  idle connection to a dead peer previously lived forever. The actor now
  closes the connection after three silent keep-alive windows.
- **Protocol deadlock from over-fragmented messages**: a message needing more
  fragments than the receive window can hold can never be reassembled.
  `send()` now rejects it up front (bounded by `min(IKCP_WND_RCV, rcv_wnd)`,
  assuming symmetric peer configuration) instead of deadlocking the stream.
- **Config validation on the main APIs**: `KcpStream::connect*` and
  `KcpListener::bind`/`with_transport` now run `KcpConfig::validate()`;
  previously only the `KcpConfig::connect()/listen()` wrappers did.
- **`mtu` ≤ 24 underflow**: `KcpEngine::new` clamps the MTU above the header
  size; `mtu - IKCP_OVERHEAD` no longer panics (debug) or wraps (release).
- **IPv6 clients**: `KcpStream::connect` binds in the target's address family;
  connecting to a v6 server previously failed on every `send_to`.
- **Buffered data lost on close**: `poll_read` drains the local read buffer
  before honoring the closed flag.

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

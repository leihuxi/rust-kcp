//! High-performance lock-free buffer pool for KCP packet allocation

use bytes::BytesMut;
use std::sync::LazyLock;

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

// Pool tier thresholds: get and put use same boundaries based on buffer_size.
//   SMALL:  buffer_size=1024   → get ≤1024, put accepts capacity 512..=2048
//   MEDIUM: buffer_size=1400   → get ≤1400, put accepts capacity 700..=2800
//   LARGE:  buffer_size=8192   → get ≤8192, put accepts capacity 4096..=16384
//   JUMBO:  buffer_size=65536  → get >8192, put accepts capacity 32768..=131072

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

/// Return a buffer to the global pool (non-blocking).
/// Uses the same tier boundaries as `try_get_buffer` for consistency.
pub fn try_put_buffer(buf: BytesMut) {
    let capacity = buf.capacity();
    if capacity <= 2048 {
        // Matches SMALL tier (buffer_size * 2 = 2048)
        SMALL_BUFFER_POOL.try_put(buf);
    } else if capacity <= 2800 {
        // Matches MEDIUM tier (buffer_size * 2 = 2800)
        MEDIUM_BUFFER_POOL.try_put(buf);
    } else if capacity <= 16384 {
        // Matches LARGE tier (buffer_size * 2 = 16384)
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

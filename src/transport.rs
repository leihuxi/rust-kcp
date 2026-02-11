//! Abstract transport layer for KCP
//!
//! The [`Transport`] trait allows KCP to run over any async datagram transport,
//! not just UDP. Enable the `tokio` feature (on by default) for the built-in
//! [`UdpTransport`] implementation backed by `tokio::net::UdpSocket`.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;

/// Boxed future returned by [`Transport`] methods.
pub type SendFuture<'a> = Pin<Box<dyn Future<Output = io::Result<usize>> + Send + 'a>>;

/// Boxed future returned by [`Transport::recv_from`].
pub type RecvFuture<'a> =
    Pin<Box<dyn Future<Output = io::Result<(usize, SocketAddr)>> + Send + 'a>>;

/// Async datagram transport used by [`KcpStream`](crate::async_kcp::KcpStream)
/// and [`KcpListener`](crate::async_kcp::KcpListener).
///
/// Implementors must provide send/receive operations addressed by `SocketAddr`.
/// The trait is object-safe so it can be used as `Arc<dyn Transport>`.
pub trait Transport: Send + Sync + 'static {
    /// Send `buf` to `target`, returning the number of bytes written.
    fn send_to<'a>(&'a self, buf: &'a [u8], target: SocketAddr) -> SendFuture<'a>;

    /// Receive a datagram into `buf`, returning `(bytes_read, source_address)`.
    fn recv_from<'a>(&'a self, buf: &'a mut [u8]) -> RecvFuture<'a>;

    /// Return the local address this transport is bound to.
    fn local_addr(&self) -> io::Result<SocketAddr>;
}

// ---------------------------------------------------------------------------
// UdpTransport — default implementation backed by tokio::net::UdpSocket
// ---------------------------------------------------------------------------

#[cfg(feature = "tokio")]
mod udp {
    use super::*;
    use tokio::net::UdpSocket;

    /// Default [`Transport`] implementation wrapping a `tokio::net::UdpSocket`.
    pub struct UdpTransport {
        socket: UdpSocket,
    }

    impl UdpTransport {
        /// Bind a new UDP socket to `addr`.
        pub async fn bind(addr: impl tokio::net::ToSocketAddrs) -> io::Result<Self> {
            let socket = UdpSocket::bind(addr).await?;
            Ok(Self { socket })
        }

        /// Wrap an existing `UdpSocket`.
        pub fn new(socket: UdpSocket) -> Self {
            Self { socket }
        }
    }

    impl Transport for UdpTransport {
        fn send_to<'a>(&'a self, buf: &'a [u8], target: SocketAddr) -> SendFuture<'a> {
            Box::pin(self.socket.send_to(buf, target))
        }

        fn recv_from<'a>(&'a self, buf: &'a mut [u8]) -> RecvFuture<'a> {
            Box::pin(self.socket.recv_from(buf))
        }

        fn local_addr(&self) -> io::Result<SocketAddr> {
            self.socket.local_addr()
        }
    }
}

#[cfg(feature = "tokio")]
pub use udp::UdpTransport;

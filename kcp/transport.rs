//! Abstract transport layer for KCP
//!
//! The [`Transport`] trait allows KCP to run over any async datagram transport,
//! not just UDP. Enable the `tokio` feature (on by default) for the built-in
//! [`UdpTransport`] implementation backed by `tokio::net::UdpSocket`.

use std::fmt::{Debug, Display};
use std::future::Future;
use std::hash::Hash;
use std::io;

/// Marker trait for address types used by [`Transport`] implementations.
///
/// Any type satisfying the required bounds automatically implements `Addr`
/// via the blanket impl. This keeps bound lists short elsewhere.
pub trait Addr: Clone + Eq + Hash + Send + Sync + Debug + Display + 'static {}

impl<T: Clone + Eq + Hash + Send + Sync + Debug + Display + 'static> Addr for T {}

/// Async datagram transport used by [`KcpStream`](crate::stream::KcpStream)
/// and [`KcpListener`](crate::listener::KcpListener).
///
/// Implementors provide send/receive operations addressed by an associated
/// [`Addr`] type. The built-in [`UdpTransport`] uses `SocketAddr`.
pub trait Transport: Send + Sync + 'static {
    /// The address type used to identify endpoints.
    type Addr: Addr;

    /// Send `buf` to `target`, returning the number of bytes written.
    fn send_to<'a>(
        &'a self,
        buf: &'a [u8],
        target: &'a Self::Addr,
    ) -> impl Future<Output = io::Result<usize>> + Send + 'a;

    /// Receive a datagram into `buf`, returning `(bytes_read, source_address)`.
    fn recv_from<'a>(
        &'a self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = io::Result<(usize, Self::Addr)>> + Send + 'a;

    /// Return the local address this transport is bound to.
    fn local_addr(&self) -> io::Result<Self::Addr>;
}

// ---------------------------------------------------------------------------
// UdpTransport — default implementation backed by tokio::net::UdpSocket
// ---------------------------------------------------------------------------

#[cfg(feature = "tokio")]
mod udp {
    use super::*;
    use std::net::SocketAddr;
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
        type Addr = SocketAddr;

        async fn send_to(&self, buf: &[u8], target: &SocketAddr) -> io::Result<usize> {
            self.socket.send_to(buf, target).await
        }

        async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
            self.socket.recv_from(buf).await
        }

        fn local_addr(&self) -> io::Result<SocketAddr> {
            self.socket.local_addr()
        }
    }
}

#[cfg(feature = "tokio")]
pub use udp::UdpTransport;

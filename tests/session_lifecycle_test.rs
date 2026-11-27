//! Session lifecycle regressions: socket release on drop (#6) and reconnecting from the same source address (#7).

use kcp_tokio::{KcpConfig, KcpListener, KcpStream, UdpTransport};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use tokio::time::timeout;

async fn wait_released(weak: &Weak<UdpTransport>, max: Duration) -> Option<Duration> {
    let start = Instant::now();
    while start.elapsed() < max {
        if weak.strong_count() == 0 {
            return Some(start.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

async fn echo_server(config: KcpConfig) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let mut listener = KcpListener::bind("127.0.0.1:0".parse().unwrap(), config)
        .await
        .unwrap();
    let addr = *listener.local_addr();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { return };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                loop {
                    match s.read(&mut buf).await {
                        Ok(n) if n > 0 => {
                            if s.write_all(&buf[..n]).await.is_err() { break; }
                            let _ = s.flush().await;
                        }
                        _ => break,
                    }
                }
            });
        }
    });
    (addr, task)
}

/// #6, live peer: drop a client stream on a quiet link — is the socket freed?
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue6_drop_releases_socket_live_peer() {
    let (addr, _srv) = echo_server(KcpConfig::new().fast_mode()).await;
    let t = Arc::new(UdpTransport::bind("127.0.0.1:0").await.unwrap());
    let weak = Arc::downgrade(&t);
    let mut s = KcpStream::connect_with_transport(t, addr, KcpConfig::new().fast_mode())
        .await
        .unwrap();
    s.send(b"hi").await.unwrap();
    assert_eq!(&timeout(Duration::from_secs(3), s.recv()).await.unwrap().unwrap().unwrap()[..], b"hi");
    tokio::time::sleep(Duration::from_millis(300)).await; // quiet link
    drop(s);
    let r = wait_released(&weak, Duration::from_secs(15)).await;
    println!("issue6 live peer: released after {:?}", r);
    assert!(r.is_some(), "socket still held 15s after drop");
}

/// #6, dead peer with unacked data: worst case for the linger.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue6_drop_releases_socket_dead_peer() {
    // Nobody listens on this port.
    let dead = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = dead.local_addr().unwrap();
    drop(dead);
    let t = Arc::new(UdpTransport::bind("127.0.0.1:0").await.unwrap());
    let weak = Arc::downgrade(&t);
    let s = KcpStream::connect_with_transport(t, addr, KcpConfig::new().fast_mode())
        .await
        .unwrap();
    s.send(b"never acked").await.unwrap();
    drop(s);
    // Bounded by the actor's 10s close linger plus one wakeup interval.
    let r = wait_released(&weak, Duration::from_secs(60)).await;
    println!("issue6 dead peer: released after {:?}", r);
    assert!(r.is_some(), "socket still held 60s after drop");
}

/// #7: reconnect from the same source address with a new conv right after the
/// previous session closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue7_reconnect_same_source_port() {
    let (addr, _srv) = echo_server(KcpConfig::new().fast_mode()).await;
    let t = Arc::new(UdpTransport::bind("127.0.0.1:0").await.unwrap());

    let mut s1 = KcpStream::connect_with_transport(t.clone(), addr, KcpConfig::new().fast_mode())
        .await
        .unwrap();
    s1.send(b"one").await.unwrap();
    assert!(timeout(Duration::from_secs(3), s1.recv()).await.is_ok());
    s1.close().await.unwrap();
    drop(s1);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut s2 = KcpStream::connect_with_transport(t, addr, KcpConfig::new().fast_mode())
        .await
        .unwrap();
    s2.send(b"two").await.unwrap();
    let start = Instant::now();
    let got = timeout(Duration::from_secs(5), s2.recv()).await;
    println!("issue7: second session echo = {:?} after {:?}", got.as_ref().map(|r| r.as_ref().ok().cloned()), start.elapsed());
    assert!(got.is_ok(), "reconnect from same source port got no reply within 5s");
}

/// #7: once the server-side session has died, reconnecting from the same
/// source address with the *same* conv must not wait for `cleanup_interval`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue7_reconnect_same_conv_after_server_stream_died() {
    let mut listener = KcpListener::bind("127.0.0.1:0".parse().unwrap(), KcpConfig::new().fast_mode())
        .await
        .unwrap();
    let addr = *listener.local_addr();
    let t = Arc::new(UdpTransport::bind("127.0.0.1:0").await.unwrap());
    const CONV: u32 = 0x1234_5678;

    let mut c1 = KcpStream::new_with_conv(t.clone(), addr, CONV, KcpConfig::new().fast_mode())
        .await
        .unwrap();
    c1.send(b"one").await.unwrap();
    let (mut s1, _) = timeout(Duration::from_secs(3), listener.accept()).await.unwrap().unwrap();
    assert_eq!(&s1.recv().await.unwrap().unwrap()[..], b"one");
    c1.close().await.unwrap();
    drop(c1);
    s1.close().await.unwrap(); // server session's actor exits; entry is now dead
    drop(s1);

    let c2 = KcpStream::new_with_conv(t, addr, CONV, KcpConfig::new().fast_mode())
        .await
        .unwrap();
    c2.send(b"two").await.unwrap();
    let (mut s2, _) = timeout(Duration::from_secs(3), listener.accept())
        .await
        .expect("same-conv reconnect not accepted within 3s")
        .unwrap();
    assert_eq!(&s2.recv().await.unwrap().unwrap()[..], b"two");
}

//! Interop regression tests against the external `tokio_kcp` crate.
//!
//! Guards the fix for issue #5: tokio_kcp clients opening with `conv=0`
//! expect the server to allocate a fresh non-zero conv and send it back,
//! otherwise their `waiting_conv` flag never clears and `send()` silently
//! refuses to transmit anything beyond the initial round trip.

use kcp_tokio::config::{KcpConfig, NodeDelayConfig};
use kcp_tokio::{KcpListener, KcpStream};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_kcp::{KcpConfig as TkConfig, KcpStream as TkStream};

/// Pick an ephemeral port for each test (the OS picks one free port per bind).
async fn bind_listener(config: KcpConfig) -> (KcpListener, SocketAddr) {
    let listener = KcpListener::bind("127.0.0.1:0".parse().unwrap(), config)
        .await
        .expect("bind listener");
    let addr = *listener.local_addr();
    (listener, addr)
}

async fn run_echo_server(mut listener: KcpListener) {
    while let Ok((mut stream, _peer)) = listener.accept().await {
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            loop {
                match stream.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if stream.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                        let _ = stream.flush().await;
                    }
                    Err(_) => break,
                }
            }
        });
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tokio_kcp_client_with_conv_zero_completes_handshake() {
    // Our server uses the realtime preset (as in the user's bug report).
    let (listener, server_addr) = bind_listener(KcpConfig::realtime()).await;
    tokio::spawn(run_echo_server(listener));

    // Give the listener background task a tick to start.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Mirrors tokio_kcp::KcpConfig::default() with the user's wnd override.
    let client_cfg = TkConfig {
        wnd_size: (128, 128),
        ..Default::default()
    };

    // conv=0 exercises the handshake path — without the server-side fix
    // this deadlocks on the second send().
    let mut stream = TkStream::connect_with_conv(&client_cfg, 0, server_addr)
        .await
        .expect("connect");

    let messages = [
        "Hello, KCP!",
        "This is a test message",
        "How are you doing?",
        "KCP is working great!",
        "Final test message",
    ];

    for msg in messages {
        stream.write_all(msg.as_bytes()).await.expect("write");
        stream.flush().await.expect("flush");

        let mut buf = [0u8; 1024];
        let n = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buf))
            .await
            .unwrap_or_else(|_| panic!("read timed out for msg {:?}", msg))
            .expect("read ok");

        assert_eq!(&buf[..n], msg.as_bytes(), "echo mismatch for {:?}", msg);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tokio_kcp_client_with_non_zero_conv_still_works() {
    // Guardrail: the conv=0 fix must not regress the simple case where the
    // client picks its own conv and the server just echoes it back verbatim.
    let (listener, server_addr) = bind_listener(KcpConfig::realtime()).await;
    tokio::spawn(run_echo_server(listener));
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client_cfg = TkConfig {
        wnd_size: (128, 128),
        ..Default::default()
    };

    let mut stream = TkStream::connect_with_conv(&client_cfg, 0x12345678, server_addr)
        .await
        .expect("connect");

    for msg in ["one", "two", "three"] {
        stream.write_all(msg.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();

        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buf))
            .await
            .expect("not timed out")
            .expect("read ok");
        assert_eq!(&buf[..n], msg.as_bytes());
    }
}

/// Our own client must keep working too — picks a random non-zero conv.
#[tokio::test(flavor = "multi_thread")]
async fn our_client_against_our_server_still_works() {
    let (listener, server_addr) = bind_listener(KcpConfig::realtime()).await;
    tokio::spawn(run_echo_server(listener));
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Normal-mode client (like tokio_kcp defaults) — not realtime.
    let client_cfg = KcpConfig::default()
        .nodelay_config(NodeDelayConfig::custom(false, 100, 0, false))
        .window_size(128, 128)
        .mtu(1400);

    let mut stream = KcpStream::connect(server_addr, client_cfg).await.unwrap();

    for msg in ["alpha", "beta", "gamma"] {
        stream.write_all(msg.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();

        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buf))
            .await
            .expect("not timed out")
            .expect("read ok");
        assert_eq!(&buf[..n], msg.as_bytes());
    }
}

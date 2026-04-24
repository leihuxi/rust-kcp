//! Repro for GitHub issue #5: compat with tokio_kcp defaults.
//! Server: realtime() (this crate). Client: mimics tokio_kcp defaults.

use kcp_tokio::config::{KcpConfig, NodeDelayConfig};
use kcp_tokio::{KcpListener, KcpStream};
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().init();

    let addr: SocketAddr = "127.0.0.1:3000".parse()?;

    // Server = realtime preset (as in user's report)
    let server_cfg = KcpConfig::realtime();
    let mut listener = KcpListener::bind(addr, server_cfg).await?;
    info!("server listening on {}", addr);

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    info!("accepted {}", peer);
                    tokio::spawn(handle_client(stream, peer));
                }
                Err(e) => {
                    error!("accept: {}", e);
                    break;
                }
            }
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Client = tokio_kcp defaults (normal mode, interval=100, wnd=(128,128), mtu=1400)
    let client_cfg = KcpConfig::default()
        .nodelay_config(NodeDelayConfig::custom(false, 100, 0, false))
        .window_size(128, 128)
        .mtu(1400);

    let mut stream = KcpStream::connect(addr, client_cfg).await?;
    info!("client connected");

    let messages = [
        "Hello, KCP!",
        "This is a test message",
        "How are you doing?",
        "KCP is working great!",
        "Final test message",
    ];

    for msg in messages {
        info!("sending: {}", msg);
        stream.write_all(msg.as_bytes()).await?;
        stream.flush().await?;

        let mut buf = [0u8; 1024];
        let read_fut = stream.read(&mut buf);
        let timed = tokio::time::timeout(std::time::Duration::from_secs(5), read_fut).await;
        match timed {
            Ok(Ok(n)) => {
                let resp = std::str::from_utf8(&buf[..n]).unwrap_or("<bad utf8>");
                info!("received echo: {} ({}B)", resp, n);
                if resp != msg {
                    error!("mismatch: expected {:?} got {:?}", msg, resp);
                    return Ok(());
                }
            }
            Ok(Err(e)) => {
                error!("read err: {}", e);
                return Ok(());
            }
            Err(_) => {
                error!("timeout after sending {:?}", msg);
                return Ok(());
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    info!("all messages ok");
    Ok(())
}

async fn handle_client(
    mut stream: KcpStream,
    peer: std::net::SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buf = [0u8; 1024];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) => {
                info!("{} disconnected", peer);
                break;
            }
            Ok(n) => {
                let msg = std::str::from_utf8(&buf[..n]).unwrap_or("<bad utf8>");
                info!("server got from {}: {}", peer, msg);
                stream.write_all(&buf[..n]).await?;
                stream.flush().await?;
                info!("server echoed to {}: {}", peer, msg);
            }
            Err(e) => {
                error!("read from {} err: {}", peer, e);
                break;
            }
        }
    }
    Ok(())
}

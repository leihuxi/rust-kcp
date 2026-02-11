//! Simple KCP echo test with auto-assigned port

use kcp_tokio::async_kcp::{KcpListener, KcpStream};
use kcp_tokio::config::KcpConfig;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::time::timeout;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_simple_kcp_echo() {
    let server_config = KcpConfig::new().fast_mode();
    let (addr_tx, addr_rx) = oneshot::channel();

    let server_handle = tokio::spawn(async move {
        let mut listener = KcpListener::bind("127.0.0.1:0".parse().unwrap(), server_config)
            .await
            .expect("Failed to bind");

        addr_tx.send(listener.local_addr()).unwrap();

        let (mut stream, _peer) = timeout(Duration::from_secs(10), listener.accept())
            .await
            .expect("Accept timeout")
            .expect("Accept failed");

        let mut buf = [0u8; 1024];
        let n = timeout(Duration::from_secs(5), stream.read(&mut buf))
            .await
            .expect("Read timeout")
            .expect("Read failed");

        stream.write_all(&buf[..n]).await.expect("Write failed");
        stream.flush().await.expect("Flush failed");

        // Explicit cleanup
        let _ = stream.close().await;
        let _ = listener.close().await;
    });

    let server_addr = addr_rx.await.unwrap();

    let client_config = KcpConfig::new().fast_mode();
    let mut client = timeout(
        Duration::from_secs(5),
        KcpStream::connect(server_addr, client_config),
    )
    .await
    .expect("Connect timeout")
    .expect("Connect failed");

    let test_msg = b"Hello from KCP client!";
    client.write_all(test_msg).await.expect("Send failed");
    client.flush().await.expect("Flush failed");

    let mut buf = [0u8; 1024];
    let n = timeout(Duration::from_secs(5), client.read(&mut buf))
        .await
        .expect("Read timeout")
        .expect("Read failed");

    assert_eq!(&buf[..n], test_msg, "Echo mismatch");

    let _ = client.close().await;
    let _ = timeout(Duration::from_secs(3), server_handle).await;
}

//! Repro for GitHub issue #5: uses external `tokio_kcp` crate as CLIENT to
//! connect to this crate's KcpListener server. Mirrors the user's exact code.

use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_kcp::{KcpConfig as TkConfig, KcpStream as TkStream};

#[tokio::main]
async fn main() {
    env_logger::init();

    let config = TkConfig {
        wnd_size: (128, 128),
        ..Default::default()
    };

    let server_addr = "127.0.0.1:3000".parse::<SocketAddr>().unwrap();

    // conv=0 triggers tokio_kcp's handshake: server must allocate a new non-zero
    // conv and send it back so the client clears `waiting_conv`. Our listener
    // does that automatically — this reproduces the original user bug report.
    let mut stream = TkStream::connect_with_conv(&config, 0, server_addr)
        .await
        .unwrap();

    let messages = [
        "Hello, KCP!",
        "This is a test message",
        "How are you doing?",
        "KCP is working great!",
        "Final test message",
    ];

    for message in messages {
        println!("Sending: {}", message);

        stream.write_all(message.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();

        let mut buffer = [0u8; 1024];
        let read_fut = stream.read(&mut buffer);
        let n = match tokio::time::timeout(std::time::Duration::from_secs(5), read_fut).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                eprintln!("read err: {}", e);
                return;
            }
            Err(_) => {
                eprintln!("TIMEOUT after sending {:?}", message);
                return;
            }
        };

        let response = String::from_utf8_lossy(&buffer[..n]);
        println!("Received echo: {}", response);
        if response == message {
            println!("OK");
        } else {
            println!("MISMATCH: expected {:?} got {:?}", message, response);
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

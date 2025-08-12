use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

#[tokio::test]
async fn test_listener_background_task() {
    // This test replicates EXACTLY how the KCP listener spawns its background task

    let server_addr = "127.0.0.1:18896";
    let socket = UdpSocket::bind(server_addr).await.expect("Failed to bind");
    let socket = Arc::new(socket);
    println!("Server bound to {}", socket.local_addr().unwrap());

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Spawn background task exactly like KCP listener
    let socket_clone = socket.clone();
    let task = tokio::spawn(async move {
        println!("Background task started");
        let mut buf = vec![0u8; 65536];
        let mut cleanup_interval = tokio::time::interval(Duration::from_secs(30));

        loop {
            tokio::select! {
                // Receive incoming packets
                recv_result = socket_clone.recv_from(&mut buf) => {
                    match recv_result {
                        Ok((size, peer_addr)) => {
                            let msg = format!("Received {} bytes from {}", size, peer_addr);
                            println!("Task: {}", msg);

                            if tx.send(msg).is_err() {
                                println!("Task: Channel closed, stopping");
                                break;
                            }
                        }
                        Err(e) => {
                            println!("Task: recv_from error: {}", e);
                            break;
                        }
                    }
                }

                // Cleanup timer (like KCP listener)
                _ = cleanup_interval.tick() => {
                    println!("Task: Cleanup tick");
                }
            }
        }
        println!("Background task ending");
    });

    // Give task time to start
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Create client and send packets
    let client = UdpSocket::bind("0.0.0.0:0")
        .await
        .expect("Failed to bind client");
    client
        .connect(server_addr)
        .await
        .expect("Failed to connect");
    println!("Client connected from {}", client.local_addr().unwrap());

    // Send test packets
    for i in 0..3 {
        let data = format!("Message {}", i);
        println!("Sending: {}", data);
        client.send(data.as_bytes()).await.expect("Failed to send");

        // Wait for task to process
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Check if we received messages
    let mut received_count = 0;
    while let Ok(msg) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
        if let Some(msg) = msg {
            println!("Main thread received: {}", msg);
            received_count += 1;
        } else {
            break;
        }
    }

    println!("Total messages received: {}", received_count);

    // Stop the background task
    task.abort();

    if received_count == 3 {
        println!("✅ SUCCESS: Background task received all packets");
    } else {
        println!(
            "❌ FAILED: Background task only received {} packets",
            received_count
        );
    }
}

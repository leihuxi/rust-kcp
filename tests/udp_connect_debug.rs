use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::timeout;

#[tokio::test]
async fn test_udp_connected_client_pattern() {
    // This test replicates the exact pattern used in KCP: 
    // Server binds and uses recv_from, Client connects and uses send
    
    let server_addr = "127.0.0.1:18892";
    
    // Server: bind and wait for packets using recv_from (like KCP listener)
    let server = UdpSocket::bind(server_addr).await.expect("Failed to bind server");
    println!("Server bound to {}", server.local_addr().unwrap());
    
    let server_task = tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        println!("Server: Calling recv_from...");
        
        match timeout(Duration::from_secs(3), server.recv_from(&mut buf)).await {
            Ok(Ok((size, peer_addr))) => {
                let msg = std::str::from_utf8(&buf[..size]).unwrap_or("<invalid>");
                println!("Server: Received '{}' from {}", msg, peer_addr);
                return true; // Success
            }
            Ok(Err(e)) => {
                println!("Server: recv_from error: {}", e);
                return false;
            }
            Err(_) => {
                println!("Server: recv_from timed out after 3 seconds");
                return false;
            }
        }
    });
    
    // Give server time to start recv_from
    tokio::time::sleep(Duration::from_millis(100)).await;
    
    // Client: connect and send (like KCP client)
    let client = UdpSocket::bind("0.0.0.0:0").await.expect("Failed to bind client");
    println!("Client bound to {}", client.local_addr().unwrap());
    
    match client.connect(server_addr).await {
        Ok(_) => {
            println!("Client: Connected to server");
        }
        Err(e) => {
            println!("Client: Failed to connect: {}", e);
            return;
        }
    }
    
    // Send data using connected socket pattern
    println!("Client: Sending data via connected socket...");
    match client.send(b"Hello from connected client").await {
        Ok(size) => {
            println!("Client: Sent {} bytes successfully", size);
        }
        Err(e) => {
            println!("Client: Failed to send: {}", e);
            return;
        }
    }
    
    // Wait for server result
    match timeout(Duration::from_secs(5), server_task).await {
        Ok(Ok(received)) => {
            if received {
                println!("✅ SUCCESS: Connected client -> recv_from server communication works");
            } else {
                println!("❌ FAILED: Server did not receive client packets");
            }
        }
        Ok(Err(e)) => {
            println!("❌ Server task panicked: {:?}", e);
        }
        Err(_) => {
            println!("❌ Server task timed out");
        }
    }
}
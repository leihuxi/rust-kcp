use tokio::net::UdpSocket;
use std::net::SocketAddr;

#[tokio::test]
async fn test_basic_udp_echo() {
    // Server
    let server_addr: SocketAddr = "127.0.0.1:19001".parse().unwrap();
    let server = UdpSocket::bind(server_addr).await.unwrap();
    
    // Client  
    let client = UdpSocket::bind("0.0.0.0:0").await.unwrap();
    client.connect(server_addr).await.unwrap();
    
    println!("Client local addr: {}", client.local_addr().unwrap());
    println!("Server addr: {}", server.local_addr().unwrap());
    
    // Send from client to server
    client.send(b"Hello").await.unwrap();
    
    // Server receives
    let mut buf = [0u8; 1024];
    let (size, peer_addr) = server.recv_from(&mut buf).await.unwrap();
    let msg = std::str::from_utf8(&buf[..size]).unwrap();
    println!("Server received '{}' from {}", msg, peer_addr);
    
    // Server responds back to same address
    server.send_to(b"Echo: Hello", peer_addr).await.unwrap();
    
    // Client receives  
    let size = client.recv(&mut buf).await.unwrap();
    let response = std::str::from_utf8(&buf[..size]).unwrap();
    println!("Client received response: '{}'", response);
    
    assert_eq!(response, "Echo: Hello");
    println!("UDP test successful!");
}
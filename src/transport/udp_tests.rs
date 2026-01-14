#[cfg(test)]
mod tests {
    use crate::control::messages::{DEFAULT_STREAM_CONFIG, MAGIC, MSG_CONFIG, MSG_HELLO, VERSION};
    use crate::transport::udp::UdpServer;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::net::UdpSocket;

    #[tokio::test]
    async fn udp_hello_receives_config() {
        // `config` is the default stream configuration used for this test.
        // Using the default keeps the test aligned with real server settings.
        // Tests often reuse defaults so they remain stable over time.
        let config = DEFAULT_STREAM_CONFIG;
        // `addr` binds to localhost with port 0 so the OS chooses a free port.
        // Port 0 is a common trick in tests to avoid conflicts.
        // The OS then tells us which port it picked.
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        // `server` is a shared UDP server instance created for the test.
        // Wrapping it in `Arc` lets the background task own a clone.
        // This mirrors how the real server shares the UDP object.
        let server = Arc::new(UdpServer::bind(addr, config).await.unwrap());
        // `server_addr` is the actual address chosen by the OS.
        // We need it so the client knows where to send the Hello packet.
        // This is a common pattern when binding to port 0.
        let server_addr = server.local_addr().unwrap();

        // `listener` is a clone so the background task can own a handle.
        // The clone is another reference to the same server, not a copy.
        // This is safe because `UdpServer` is wrapped in `Arc`.
        let listener = Arc::clone(&server);
        tokio::spawn(async move {
            let _ = listener.run_listener().await;
        });

        // `client` is a UDP socket that will send a Hello packet.
        // It binds to port 0 so the OS chooses a free port.
        // This keeps tests isolated from each other.
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        // `hello` is a byte buffer containing a properly formatted Hello packet.
        // This mimics what a real client would send.
        // Building it manually keeps the test explicit and readable.
        let mut hello = Vec::new();
        hello.extend_from_slice(&MAGIC);
        hello.push(VERSION);
        hello.push(MSG_HELLO);
        client.send_to(&hello, server_addr).await.unwrap();

        // `buf` is a buffer for the server's response.
        // The server should reply with a config packet in response to Hello.
        // If it does, the packet will have the expected header bytes.
        let mut buf = [0u8; 256];
        let (len, _) = client.recv_from(&mut buf).await.unwrap();
        assert!(len >= 6);
        assert_eq!(&buf[0..4], MAGIC.as_slice());
        assert_eq!(buf[4], VERSION);
        assert_eq!(buf[5], MSG_CONFIG);
    }
}

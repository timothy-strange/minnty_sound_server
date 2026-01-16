#[cfg(test)]
mod tests {
    use crate::control::messages::{DEFAULT_STREAM_CONFIG, MAGIC, MSG_CONFIG, MSG_HELLO, VERSION};
    use crate::transport::udp::UdpServer;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::net::UdpSocket;

    #[tokio::test]
    async fn udp_hello_receives_config() {
        let config = DEFAULT_STREAM_CONFIG;
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        let server = Arc::new(UdpServer::bind(addr, config).await.unwrap());
        let server_addr = server.local_addr().unwrap();

        let listener = Arc::clone(&server);
        tokio::spawn(async move {
            let _ = listener.run_listener().await;
        });

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut hello = Vec::new();
        hello.extend_from_slice(&MAGIC);
        hello.push(VERSION);
        hello.push(MSG_HELLO);
        client.send_to(&hello, server_addr).await.unwrap();

        let mut buf = [0u8; 256];
        let (len, _) = client.recv_from(&mut buf).await.unwrap();
        assert!(len >= 6);
        assert_eq!(&buf[0..4], MAGIC.as_slice());
        assert_eq!(buf[4], VERSION);
        assert_eq!(buf[5], MSG_CONFIG);
    }
}
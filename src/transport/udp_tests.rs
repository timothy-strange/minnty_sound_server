#[cfg(test)]
mod tests {
    use crate::control::messages::{
        DEFAULT_STREAM_CONFIG, MAGIC, MSG_CONFIG, MSG_HELLO, MSG_KEEPALIVE, MSG_STATUS,
        MSG_STATUS_REQUEST, MSG_TIME_SYNC_REQUEST, MSG_TIME_SYNC_RESPONSE, VERSION,
    };
    use crate::transport::udp::UdpServer;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::time::{Duration, timeout};
    use tokio::net::UdpSocket;

    #[tokio::test]
    async fn udp_hello_receives_config() {
        let config = DEFAULT_STREAM_CONFIG;
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        let server = Arc::new(UdpServer::bind(addr, config, 123).await.unwrap());
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

    #[tokio::test]
    async fn udp_status_request_receives_status() {
        let config = DEFAULT_STREAM_CONFIG;
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        let session_id = 0x1122_3344_5566_7788;
        let server = Arc::new(UdpServer::bind(addr, config, session_id).await.unwrap());
        let server_addr = server.local_addr().unwrap();
        server.set_streaming(true);

        let listener = Arc::clone(&server);
        tokio::spawn(async move {
            let _ = listener.run_listener().await;
        });

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut status_request = Vec::new();
        status_request.extend_from_slice(&MAGIC);
        status_request.push(VERSION);
        status_request.push(MSG_STATUS_REQUEST);
        client.send_to(&status_request, server_addr).await.unwrap();

        let mut buf = [0u8; 256];
        let (len, _) = client.recv_from(&mut buf).await.unwrap();
        assert_eq!(len, 15);
        assert_eq!(&buf[0..4], MAGIC.as_slice());
        assert_eq!(buf[4], VERSION);
        assert_eq!(buf[5], MSG_STATUS);
        assert_eq!(buf[6], 1);
        let returned_session = u64::from_be_bytes([
            buf[7], buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14],
        ]);
        assert_eq!(returned_session, session_id);
    }

    #[tokio::test]
    async fn udp_keepalive_from_unknown_sender_does_not_reply() {
        let config = DEFAULT_STREAM_CONFIG;
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        let server = Arc::new(UdpServer::bind(addr, config, 456).await.unwrap());
        let server_addr = server.local_addr().unwrap();

        let listener = Arc::clone(&server);
        tokio::spawn(async move {
            let _ = listener.run_listener().await;
        });

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut keepalive = Vec::new();
        keepalive.extend_from_slice(&MAGIC);
        keepalive.push(VERSION);
        keepalive.push(MSG_KEEPALIVE);
        client.send_to(&keepalive, server_addr).await.unwrap();

        let mut buf = [0u8; 256];
        let recv = timeout(Duration::from_millis(200), client.recv_from(&mut buf)).await;
        assert!(recv.is_err(), "unexpected response to unknown KEEPALIVE sender");
    }

    #[tokio::test]
    async fn udp_time_sync_request_receives_time_sync_response() {
        let config = DEFAULT_STREAM_CONFIG;
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        let server = Arc::new(UdpServer::bind(addr, config, 789).await.unwrap());
        let server_addr = server.local_addr().unwrap();

        let listener = Arc::clone(&server);
        tokio::spawn(async move {
            let _ = listener.run_listener().await;
        });

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client_send_ms = 0x0102_0304_0506_0708u64;
        let mut request = Vec::new();
        request.extend_from_slice(&MAGIC);
        request.push(VERSION);
        request.push(MSG_TIME_SYNC_REQUEST);
        request.extend_from_slice(&client_send_ms.to_be_bytes());
        client.send_to(&request, server_addr).await.unwrap();

        let mut buf = [0u8; 256];
        let (len, _) = client.recv_from(&mut buf).await.unwrap();
        assert_eq!(len, 30);
        assert_eq!(&buf[0..4], MAGIC.as_slice());
        assert_eq!(buf[4], VERSION);
        assert_eq!(buf[5], MSG_TIME_SYNC_RESPONSE);
        let returned_client_send = u64::from_be_bytes([
            buf[6], buf[7], buf[8], buf[9], buf[10], buf[11], buf[12], buf[13],
        ]);
        let server_receive = u64::from_be_bytes([
            buf[14], buf[15], buf[16], buf[17], buf[18], buf[19], buf[20], buf[21],
        ]);
        let server_send = u64::from_be_bytes([
            buf[22], buf[23], buf[24], buf[25], buf[26], buf[27], buf[28], buf[29],
        ]);
        assert_eq!(returned_client_send, client_send_ms);
        assert!(server_send >= server_receive);
    }
}

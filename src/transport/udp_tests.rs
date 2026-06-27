#[cfg(test)]
mod tests {
    use crate::control::messages::{
        DEFAULT_STREAM_CONFIG, MAGIC, MSG_CONFIG, MSG_HELLO, MSG_KEEPALIVE, MSG_MEDIA_CONTROL,
        MSG_NOW_PLAYING, MSG_STATUS, MSG_STATUS_REQUEST, MSG_TIME_SYNC_REQUEST,
        MSG_TIME_SYNC_RESPONSE, MediaCommand, PlaybackStatus, VERSION, build_media_control_packet,
    };
    use crate::media_control::MediaController;
    use crate::transport::udp::UdpServer;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::Mutex;
    use tokio::net::UdpSocket;
    use tokio::time::{Duration, timeout};

    #[derive(Default)]
    struct RecordingMediaController {
        commands: Mutex<Vec<(MediaCommand, i64)>>,
        volume_adjustments: Mutex<Vec<(MediaCommand, Option<String>)>>,
    }

    impl MediaController for RecordingMediaController {
        fn handle(&self, command: MediaCommand, argument: i64) {
            self.commands.lock().unwrap().push((command, argument));
        }

        fn adjust_volume(&self, command: MediaCommand, sink: Option<&str>) {
            self.volume_adjustments
                .lock()
                .unwrap()
                .push((command, sink.map(str::to_string)));
        }

        fn now_playing(&self) -> Option<crate::control::messages::NowPlayingMetadata> {
            None
        }
    }

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
        // Fixed prefix (15 bytes) plus the trailing name field (len u16 + name bytes).
        assert!(len >= 17);
        assert_eq!(&buf[0..4], MAGIC.as_slice());
        assert_eq!(buf[4], VERSION);
        assert_eq!(buf[5], MSG_STATUS);
        assert_eq!(buf[6], 1);
        let returned_session = u64::from_be_bytes([
            buf[7], buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14],
        ]);
        assert_eq!(returned_session, session_id);
        let name_len = u16::from_be_bytes([buf[15], buf[16]]) as usize;
        assert_eq!(len, 17 + name_len);
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
        assert!(
            recv.is_err(),
            "unexpected response to unknown KEEPALIVE sender"
        );
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

    #[tokio::test]
    async fn udp_media_transport_command_invokes_controller() {
        let config = DEFAULT_STREAM_CONFIG;
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        let media_controller = Arc::new(RecordingMediaController::default());
        let server = Arc::new(
            UdpServer::bind_with_media_controller(
                addr,
                config,
                789,
                media_controller.clone(),
                Arc::new(crate::server_state::ServerState::new()),
            )
            .await
            .unwrap(),
        );
        let server_addr = server.local_addr().unwrap();

        let listener = Arc::clone(&server);
        tokio::spawn(async move {
            let _ = listener.run_listener().await;
        });

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(
                &build_media_control_packet(MediaCommand::SeekRelativeMs, -30_000),
                server_addr,
            )
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        let commands = media_controller.commands.lock().unwrap();
        assert_eq!(
            commands.as_slice(),
            &[(MediaCommand::SeekRelativeMs, -30_000)]
        );
    }

    #[tokio::test]
    async fn udp_volume_command_forwards_to_other_clients_only() {
        let config = DEFAULT_STREAM_CONFIG;
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        let server = Arc::new(UdpServer::bind(addr, config, 789).await.unwrap());
        let server_addr = server.local_addr().unwrap();

        let listener = Arc::clone(&server);
        tokio::spawn(async move {
            let _ = listener.run_listener().await;
        });

        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut hello = Vec::new();
        hello.extend_from_slice(&MAGIC);
        hello.push(VERSION);
        hello.push(MSG_HELLO);
        sender.send_to(&hello, server_addr).await.unwrap();
        receiver.send_to(&hello, server_addr).await.unwrap();

        let mut buf = [0u8; 256];
        let _ = sender.recv_from(&mut buf).await.unwrap();
        let _ = receiver.recv_from(&mut buf).await.unwrap();

        sender
            .send_to(
                &build_media_control_packet(MediaCommand::VolumeUp, 0),
                server_addr,
            )
            .await
            .unwrap();

        let (len, _) = receiver.recv_from(&mut buf).await.unwrap();
        assert_eq!(len, 15);
        assert_eq!(&buf[0..4], MAGIC.as_slice());
        assert_eq!(buf[4], VERSION);
        assert_eq!(buf[5], MSG_MEDIA_CONTROL);
        assert_eq!(buf[6], MediaCommand::VolumeUp.code());

        let sender_recv = timeout(Duration::from_millis(200), sender.recv_from(&mut buf)).await;
        assert!(
            sender_recv.is_err(),
            "sender should not receive its own forwarded volume command"
        );
    }

    #[tokio::test]
    async fn udp_volume_command_adjusts_server_volume_when_enabled() {
        let config = DEFAULT_STREAM_CONFIG;
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        let media_controller = Arc::new(RecordingMediaController::default());
        let server_state = Arc::new(crate::server_state::ServerState::new());
        server_state.set_change_server_volume_from_clients(true);
        server_state
            .set_current_sink(Some("alsa_output.test".to_string()))
            .await;
        let server = Arc::new(
            UdpServer::bind_with_media_controller(
                addr,
                config,
                789,
                media_controller.clone(),
                server_state,
            )
            .await
            .unwrap(),
        );
        let server_addr = server.local_addr().unwrap();

        let listener = Arc::clone(&server);
        tokio::spawn(async move {
            let _ = listener.run_listener().await;
        });

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(
                &build_media_control_packet(MediaCommand::VolumeDown, 0),
                server_addr,
            )
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        let adjustments = media_controller.volume_adjustments.lock().unwrap();
        assert_eq!(
            adjustments.as_slice(),
            &[(
                MediaCommand::VolumeDown,
                Some("alsa_output.test".to_string())
            )]
        );
    }

    #[tokio::test]
    async fn udp_hello_receives_latest_calibration_now_playing() {
        let config = DEFAULT_STREAM_CONFIG;
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        let server = Arc::new(UdpServer::bind(addr, config, 789).await.unwrap());
        let server_addr = server.local_addr().unwrap();
        server.publish_calibration_now_playing().await;

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
        let _ = client.recv_from(&mut buf).await.unwrap();
        let (len, _) = client.recv_from(&mut buf).await.unwrap();

        assert_eq!(&buf[0..4], MAGIC.as_slice());
        assert_eq!(buf[4], VERSION);
        assert_eq!(buf[5], MSG_NOW_PLAYING);
        assert_eq!(buf[14], PlaybackStatus::Playing.code());
        let artist_len = u16::from_be_bytes([buf[15], buf[16]]) as usize;
        let title_len = u16::from_be_bytes([buf[17], buf[18]]) as usize;
        assert_eq!(artist_len, 0);
        assert_eq!(title_len, "Calibration stream".len());
        assert_eq!(
            std::str::from_utf8(&buf[19..19 + title_len]).unwrap(),
            "Calibration stream"
        );
        assert_eq!(len, 19 + title_len + 16);
    }
}

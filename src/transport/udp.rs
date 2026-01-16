use crate::control::messages::{
    ControlMessage, StreamConfig, build_config_packet, parse_control_packet,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

struct ClientInfo {
    last_seen: Instant,
}

pub struct UdpServer {
    socket: Arc<UdpSocket>,
    clients: Arc<Mutex<HashMap<SocketAddr, ClientInfo>>>,
    config: StreamConfig,
}

impl UdpServer {
    pub async fn bind(
        addr: SocketAddr,
        config: StreamConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let socket = UdpSocket::bind(addr).await?;
        Ok(Self {
            socket: Arc::new(socket),
            clients: Arc::new(Mutex::new(HashMap::new())),
            config,
        })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub async fn run_listener(self: Arc<Self>) -> Result<(), Box<dyn std::error::Error>> {
        let mut buffer = vec![0u8; 1024];
        loop {
            let (len, addr) = self.socket.recv_from(&mut buffer).await?;
            if let Some(message) = parse_control_packet(&buffer[..len]) {
                self.register_client(addr).await;
                if matches!(message, ControlMessage::Hello) {
                    let packet = build_config_packet(self.config);
                    let _ = self.socket.send_to(&packet, addr).await;
                }
            }
        }
    }

    pub async fn send_to_clients(&self, packet: &[u8]) {
        let addresses = {
            let mut clients = self.clients.lock().await;
            let now = Instant::now();
            clients.retain(|_, info| now.duration_since(info.last_seen) <= CLIENT_TIMEOUT);
            clients.keys().copied().collect::<Vec<_>>()
        };

        for addr in addresses {
            let _ = self.socket.send_to(packet, addr).await;
        }
    }

    async fn register_client(&self, addr: SocketAddr) {
        let mut clients = self.clients.lock().await;
        clients.insert(
            addr,
            ClientInfo {
                last_seen: Instant::now(),
            },
        );
    }
}
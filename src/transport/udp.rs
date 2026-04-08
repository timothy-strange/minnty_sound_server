use crate::control::messages::{
    ControlMessage, StreamConfig, build_config_packet, build_status_packet, parse_control_packet,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

struct ClientInfo {
    last_seen: Instant,
}

struct UdpStats {
    frames_sent_window: u64,
    send_errors_window: u64,
    last_summary: Instant,
}

pub struct UdpServer {
    socket: Arc<UdpSocket>,
    clients: Arc<Mutex<HashMap<SocketAddr, ClientInfo>>>,
    config: StreamConfig,
    session_id: u64,
    streaming: Arc<AtomicBool>,
    stats: Arc<Mutex<UdpStats>>,
}

impl UdpServer {
    pub async fn bind(
        addr: SocketAddr,
        config: StreamConfig,
        session_id: u64,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let socket = UdpSocket::bind(addr).await?;
        Ok(Self {
            socket: Arc::new(socket),
            clients: Arc::new(Mutex::new(HashMap::new())),
            config,
            session_id,
            streaming: Arc::new(AtomicBool::new(false)),
            stats: Arc::new(Mutex::new(UdpStats {
                frames_sent_window: 0,
                send_errors_window: 0,
                last_summary: Instant::now(),
            })),
        })
    }

    pub async fn run_listener(self: Arc<Self>) -> Result<(), Box<dyn std::error::Error>> {
        let mut buffer = vec![0u8; 1024];
        loop {
            let (len, addr) = self.socket.recv_from(&mut buffer).await?;
            if let Some(message) = parse_control_packet(&buffer[..len]) {
                match message {
                    ControlMessage::Hello => {
                        self.register_playback_client(addr).await;
                        let packet = build_config_packet(self.config);
                        let _ = self.socket.send_to(&packet, addr).await;
                    }
                    ControlMessage::KeepAlive => {
                        self.refresh_playback_client(addr).await;
                    }
                    ControlMessage::StatusRequest => {
                        let packet = build_status_packet(
                            self.streaming.load(Ordering::Relaxed),
                            self.session_id,
                        );
                        let _ = self.socket.send_to(&packet, addr).await;
                    }
                    ControlMessage::Status => {}
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub async fn send_to_clients(&self, packet: &[u8]) {
        let addresses = {
            let mut clients = self.clients.lock().await;
            let now = Instant::now();
            clients.retain(|_, info| now.duration_since(info.last_seen) <= CLIENT_TIMEOUT);
            clients.keys().copied().collect::<Vec<_>>()
        };

        let client_count = addresses.len();
        let mut send_errors = 0u64;

        for addr in addresses {
            if self.socket.send_to(packet, addr).await.is_err() {
                send_errors += 1;
            }
        }

        let now = Instant::now();
        let mut stats = self.stats.lock().await;
        stats.frames_sent_window += 1;
        stats.send_errors_window += send_errors;

        let mut summary: Option<(u64, u64, usize)> = None;
        if now.duration_since(stats.last_summary) >= Duration::from_secs(2) {
            summary = Some((
                stats.frames_sent_window,
                stats.send_errors_window,
                client_count,
            ));
            stats.frames_sent_window = 0;
            stats.send_errors_window = 0;
            stats.last_summary = now;
        }
        drop(stats);

        if let Some((frames_sent, send_errors_window, active_clients)) = summary {
            crate::log_info!(
                "udp diag: framesSent={} sendErrors={} activeClients={}",
                frames_sent,
                send_errors_window,
                active_clients
            );
        }
    }

    pub fn set_streaming(&self, streaming: bool) {
        self.streaming.store(streaming, Ordering::Relaxed);
    }

    async fn register_playback_client(&self, addr: SocketAddr) {
        let mut clients = self.clients.lock().await;
        clients.insert(
            addr,
            ClientInfo {
                last_seen: Instant::now(),
            },
        );
    }

    async fn refresh_playback_client(&self, addr: SocketAddr) {
        let mut clients = self.clients.lock().await;
        if let Some(client) = clients.get_mut(&addr) {
            client.last_seen = Instant::now();
        }
    }
}

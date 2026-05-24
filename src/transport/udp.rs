use crate::control::messages::{
    ControlMessage, MediaCommand, NowPlayingMetadata, PlaybackStatus, StreamConfig,
    build_config_packet, build_media_control_packet, build_now_playing_packet, build_status_packet,
    build_time_sync_response_packet, parse_control_packet,
};
use crate::media_control::{MediaController, PlatformMediaController};
use crate::server_state::ServerState;
use bytes::Bytes;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::time::sleep;

const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);
const METADATA_POLL_INTERVAL: Duration = Duration::from_secs(2);

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
    media_controller: Arc<dyn MediaController>,
    server_state: Arc<ServerState>,
    latest_metadata_packet: Arc<Mutex<Option<Bytes>>>,
}

impl UdpServer {
    #[cfg(test)]
    pub async fn bind(
        addr: SocketAddr,
        config: StreamConfig,
        session_id: u64,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::bind_with_state(addr, config, session_id, Arc::new(ServerState::new())).await
    }

    pub async fn bind_with_state(
        addr: SocketAddr,
        config: StreamConfig,
        session_id: u64,
        server_state: Arc<ServerState>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::bind_with_media_controller(
            addr,
            config,
            session_id,
            Arc::new(PlatformMediaController),
            server_state,
        )
        .await
    }

    pub async fn bind_with_media_controller(
        addr: SocketAddr,
        config: StreamConfig,
        session_id: u64,
        media_controller: Arc<dyn MediaController>,
        server_state: Arc<ServerState>,
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
            media_controller,
            server_state,
            latest_metadata_packet: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn run_listener(self: Arc<Self>) -> Result<(), Box<dyn std::error::Error>> {
        let mut buffer = vec![0u8; 1024];
        loop {
            let (len, addr) = self.socket.recv_from(&mut buffer).await?;
            let server_receive_ms = current_wall_clock_ms();
            if let Some(message) = parse_control_packet(&buffer[..len]) {
                match message {
                    ControlMessage::Hello => {
                        self.register_playback_client(addr).await;
                        let _ = self
                            .socket
                            .send_to(&build_config_packet(self.config), addr)
                            .await;
                        if let Some(packet) = self.latest_metadata_packet.lock().await.clone() {
                            let _ = self.socket.send_to(&packet, addr).await;
                        }
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
                    ControlMessage::TimeSyncRequest { client_send_ms } => {
                        let packet = build_time_sync_response_packet(
                            client_send_ms,
                            server_receive_ms,
                            current_wall_clock_ms(),
                        );
                        let _ = self.socket.send_to(&packet, addr).await;
                    }
                    ControlMessage::MediaControl { command, argument } => {
                        self.handle_media_control(addr, command, argument).await;
                    }
                }
            }
        }
    }

    pub fn start_metadata_broadcast(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut sequence = 0u64;
            let mut last_metadata: Option<NowPlayingMetadata> = None;
            loop {
                sleep(METADATA_POLL_INTERVAL).await;
                let controller = Arc::clone(&self.media_controller);
                let metadata = tokio::task::spawn_blocking(move || controller.now_playing())
                    .await
                    .unwrap_or(None);
                if metadata == last_metadata
                    && !matches!(
                        metadata.as_ref().map(|m| m.playback_status),
                        Some(PlaybackStatus::Playing)
                    )
                {
                    continue;
                }
                sequence = sequence.wrapping_add(1);
                let packet_metadata = metadata.clone().unwrap_or(NowPlayingMetadata {
                    artist: String::new(),
                    title: String::new(),
                    playback_status: PlaybackStatus::Unknown,
                    position_ms: None,
                    duration_ms: None,
                    track_id: None,
                });
                let packet = build_now_playing_packet(sequence, &packet_metadata);
                *self.latest_metadata_packet.lock().await = Some(packet.clone());
                let (sent, errors) = self.send_packet_to_clients(&packet).await;
                crate::log_info!(
                    "now playing changed sequence={} artist=\"{}\" title=\"{}\" status={:?} positionMs={:?} durationMs={:?} clients={} sendErrors={}",
                    sequence,
                    packet_metadata.artist,
                    packet_metadata.title,
                    packet_metadata.playback_status,
                    packet_metadata.position_ms,
                    packet_metadata.duration_ms,
                    sent,
                    errors
                );
                last_metadata = metadata;
            }
        });
    }

    #[cfg(test)]
    pub(crate) fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub async fn send_to_clients(&self, packet: &[u8]) {
        let addresses = self.active_client_addresses(None).await;

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

    async fn handle_media_control(&self, sender: SocketAddr, command: MediaCommand, argument: i64) {
        let registered_clients = self.active_client_addresses(None).await.len();
        crate::log_info!(
            "media control received command={:?} argument={} sender={} registeredClients={}",
            command,
            argument,
            sender,
            registered_clients
        );
        if command.is_volume() {
            let packet = build_media_control_packet(command, argument);
            let (forwarded, errors) = self.send_to_clients_except(&packet, sender).await;
            crate::log_info!(
                "media control forwarded command={:?} sender={} forwardedClients={} sendErrors={}",
                command,
                sender,
                forwarded,
                errors
            );
            if self.server_state.change_server_volume_from_clients() {
                let sink = self.server_state.current_sink().await;
                self.media_controller
                    .adjust_volume(command, sink.as_deref());
            }
        } else {
            crate::log_info!(
                "media control dispatching command={:?} argument={}",
                command,
                argument
            );
            self.media_controller.handle(command, argument);
        }
    }

    async fn send_to_clients_except(&self, packet: &[u8], excluded: SocketAddr) -> (usize, u64) {
        let addresses = self.active_client_addresses(Some(excluded)).await;
        self.send_packet_to_addresses(packet, addresses).await
    }

    async fn send_packet_to_clients(&self, packet: &[u8]) -> (usize, u64) {
        let addresses = self.active_client_addresses(None).await;
        self.send_packet_to_addresses(packet, addresses).await
    }

    async fn send_packet_to_addresses(
        &self,
        packet: &[u8],
        addresses: Vec<SocketAddr>,
    ) -> (usize, u64) {
        let forwarded = addresses.len();
        let mut send_errors = 0u64;
        for addr in addresses {
            if self.socket.send_to(packet, addr).await.is_err() {
                send_errors += 1;
            }
        }
        (forwarded, send_errors)
    }

    async fn active_client_addresses(&self, excluded: Option<SocketAddr>) -> Vec<SocketAddr> {
        let mut clients = self.clients.lock().await;
        let now = Instant::now();
        clients.retain(|_, info| now.duration_since(info.last_seen) <= CLIENT_TIMEOUT);
        clients
            .keys()
            .copied()
            .filter(|addr| Some(*addr) != excluded)
            .collect::<Vec<_>>()
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

fn current_wall_clock_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

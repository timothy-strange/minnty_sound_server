use crate::control::messages::{
    ControlMessage, StreamConfig, build_config_packet, parse_control_packet,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

// `CLIENT_TIMEOUT` is how long a client can be quiet before being removed.
// This prevents the server from keeping a forever‑growing list of clients.
// Timeouts are a common safety mechanism in network servers.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

// `ClientInfo` stores information about a single connected client.
// Right now it only stores the last time the client sent a message.
// It could be expanded later if more metadata is needed.
struct ClientInfo {
    // `last_seen` is the most recent time a control packet was received.
    // `Instant` is a monotonic clock, meaning it always moves forward.
    // Using `Instant` avoids problems with system clock changes.
    last_seen: Instant,
}

// `UdpServer` owns the UDP socket and tracks active clients.
// It wraps a low‑level socket with higher‑level behavior.
// This is a classic example of building application‑specific logic on top of a library.
pub struct UdpServer {
    // `socket` is the actual UDP socket wrapped in `Arc` so multiple tasks can use it.
    // `Arc` stands for “atomic reference counted,” meaning thread‑safe sharing.
    // The socket stays alive as long as at least one `Arc` clone exists.
    socket: Arc<UdpSocket>,
    // `clients` is a map from client addresses to their last seen time, protected
    // by a mutex because it can be updated from multiple tasks.
    // A mutex is a lock that ensures only one task updates the map at a time.
    // The map lives on the heap and is shared across tasks.
    clients: Arc<Mutex<HashMap<SocketAddr, ClientInfo>>>,
    // `config` stores stream settings used when replying to clients.
    // It is copied into the server so the listener has easy access.
    // Keeping it here avoids passing config around everywhere.
    config: StreamConfig,
}

impl UdpServer {
    // This binds a UDP socket and constructs the server object.
    // Binding means asking the OS to reserve a port for this program.
    // The async `bind` lets the runtime wait without blocking.
    pub async fn bind(
        // `addr` is the IP and port to bind the UDP socket to.
        // The address is a combination of a numeric IP and a port number.
        // Binding to 0.0.0.0 means “listen on all interfaces.”
        addr: SocketAddr,
        // `config` is stored so the server can reply with stream settings.
        // Passing it by value is fine because `StreamConfig` is small and Copy.
        // This keeps the API clean for callers.
        config: StreamConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // `socket` is created asynchronously. `await` pauses until the bind completes.
        // If binding fails (for example, the port is in use), the error is returned.
        // The `?` operator forwards that error to the caller.
        let socket = UdpSocket::bind(addr).await?;
        Ok(Self {
            socket: Arc::new(socket),
            clients: Arc::new(Mutex::new(HashMap::new())),
            config,
        })
    }

    // This returns the local address the socket is actually bound to.
    // This is useful when the OS chose a random free port.
    // It can also be used for debugging or logging.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    // This listens forever for UDP control packets from clients.
    // It is intended to run in a background task.
    // The loop only ends if an error occurs.
    pub async fn run_listener(self: Arc<Self>) -> Result<(), Box<dyn std::error::Error>> {
        // `buffer` is a reusable byte buffer for incoming packets.
        // Reusing the buffer avoids allocating new memory for every packet.
        // The size 1024 is enough for these small control packets.
        let mut buffer = vec![0u8; 1024];
        loop {
            // `recv_from` waits for a packet and returns its length and sender address.
            // This is an async wait, so the thread can do other tasks in the meantime.
            // The sender address tells us who to reply to if needed.
            let (len, addr) = self.socket.recv_from(&mut buffer).await?;
            // `parse_control_packet` checks the bytes and returns a control message.
            // If parsing fails, the packet is ignored.
            // This helps protect the server from random or malformed traffic.
            if let Some(message) = parse_control_packet(&buffer[..len]) {
                // Record this client as active.
                // This updates the last‑seen time so the client isn’t timed out.
                // It also ensures future audio packets are sent to this client.
                self.register_client(addr).await;
                // If this is a Hello message, send back the stream configuration.
                // This is how clients learn the correct audio settings.
                // The reply is sent over UDP to the same address.
                if matches!(message, ControlMessage::Hello) {
                    let packet = build_config_packet(self.config);
                    let _ = self.socket.send_to(&packet, addr).await;
                }
            }
        }
    }

    // This sends a packet to every client that is still considered active.
    // It also removes clients that have not sent keepalives recently.
    // This keeps the client list accurate and avoids wasting bandwidth.
    pub async fn send_to_clients(&self, packet: &[u8]) {
        // `addresses` is built by removing expired clients from the map.
        // The inner block limits how long the mutex is held.
        // Collecting addresses into a vector lets us release the lock early.
        let addresses = {
            let mut clients = self.clients.lock().await;
            let now = Instant::now();
            clients.retain(|_, info| now.duration_since(info.last_seen) <= CLIENT_TIMEOUT);
            clients.keys().copied().collect::<Vec<_>>()
        };

        // Send the packet to each active client.
        // The send result is ignored because losing a packet is acceptable in UDP.
        // UDP is connectionless and does not guarantee delivery.
        for addr in addresses {
            let _ = self.socket.send_to(packet, addr).await;
        }
    }

    // This records a client as active and stores the current time.
    // It is called whenever a control packet is received.
    // Updating the map inside a mutex ensures thread‑safe access.
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

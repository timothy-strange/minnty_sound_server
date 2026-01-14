use crate::control::messages::{MAGIC, MSG_HELLO, MSG_KEEPALIVE, StreamConfig, VERSION};
use opus::{Channels, Decoder};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

// `HEADER_SIZE` is the number of bytes in the stream packet header before the
// encoded audio payload begins.
// We use this to validate incoming packets before decoding them.
// It keeps the parsing logic straightforward.
const HEADER_SIZE: usize = 1 + 4 + 8 + 2;

// `MonitorStatus` is what the UI sees when it asks about monitoring.
// It is a tiny summary, so the UI doesn’t need to know internal details.
// This keeps the API clean and beginner‑friendly.
#[derive(Clone, Copy, Debug)]
pub struct MonitorStatus {
    // `active` tells the UI whether the monitor task is running.
    // A simple boolean is easy to render as “on/off” in the UI.
    // It also helps the UI decide which buttons to show.
    pub active: bool,
    // `peak` is the latest peak level, stored as a floating‑point number.
    // The value is normalized between 0.0 and 1.0.
    // This makes it easy to draw a meter bar.
    pub peak: f32,
}

// `MonitorManager` starts and stops a small UDP client that listens to the
// stream and calculates peak levels locally.
// It acts like a “mini client” inside the server for monitoring.
// Keeping it separate makes the streaming logic cleaner.
pub struct MonitorManager {
    // `config` holds the stream format so the monitor can decode packets.
    // This is a copy of the shared stream settings.
    // It is small enough to store directly.
    config: StreamConfig,
    // `peak` is a shared atomic holding the latest peak value as raw bits.
    // Atomic values can be updated from multiple threads without locks.
    // The float is stored as bits because atomic floats are not available in Rust.
    peak: Arc<AtomicU32>,
    // `state` holds runtime information and is protected by a mutex.
    // The mutex prevents race conditions between start/stop requests.
    // Using a mutex here keeps the logic straightforward.
    state: Mutex<MonitorState>,
}

// `MonitorState` holds the running task and the channel used to stop it.
// It is not exposed publicly because it is just internal bookkeeping.
// Keeping it private prevents accidental misuse.
struct MonitorState {
    // `active` is true if the monitor task is currently running.
    // This prevents multiple monitor tasks from being started at once.
    // It also helps the UI show the correct status.
    active: bool,
    // `stop_tx` is a one‑time channel to signal the task to stop.
    // A oneshot channel only sends one message, which fits this use case.
    // It is stored in an Option because it may or may not exist.
    stop_tx: Option<oneshot::Sender<()>>,
    // `task` holds the async task handle so it can be awaited.
    // This lets the manager wait for the task to finish cleanly.
    // Task handles are cheap and do not block other work.
    task: Option<JoinHandle<()>>,
}

impl MonitorManager {
    // This creates a new manager and initializes the peak value to zero.
    // The peak is stored as raw bits in an atomic for thread safety.
    // The manager starts in an “inactive” state.
    pub fn new(config: StreamConfig) -> Self {
        Self {
            config,
            peak: Arc::new(AtomicU32::new(0f32.to_bits())),
            state: Mutex::new(MonitorState {
                active: false,
                stop_tx: None,
                task: None,
            }),
        }
    }

    // This starts the monitor task, which listens to the UDP stream.
    // It returns an error if monitoring is already active.
    // This avoids creating duplicate monitoring tasks.
    pub async fn start(&self) -> Result<(), String> {
        // Lock the state so it can be safely updated.
        // The lock is released automatically at the end of the scope.
        // This is Rust’s RAII pattern in action.
        let mut state = self.state.lock().await;
        if state.active {
            return Err("Monitor already running".to_string());
        }

        // Create a one‑shot channel used to stop the task later.
        // The sender is kept in state; the receiver is passed to the task.
        // This is a simple and reliable stop mechanism.
        let (stop_tx, mut stop_rx) = oneshot::channel();
        // Clone the peak handle so the task can update the shared value.
        // Cloning an `Arc` does not copy the data, just the pointer.
        // This keeps updates consistent across threads.
        let peak = Arc::clone(&self.peak);
        // Copy the config so the task has the stream settings it needs.
        // This avoids borrowing issues because the task lives independently.
        // Copying is cheap because `StreamConfig` is small.
        let config = self.config;

        // Spawn the monitoring task on Tokio's runtime.
        // The task runs concurrently with the rest of the server.
        // Using `tokio::spawn` keeps the main flow non‑blocking.
        let task = tokio::spawn(async move {
            run_monitor(config, peak, &mut stop_rx).await;
        });

        // Update state so the UI can see that monitoring is active.
        // This is done while holding the lock for consistency.
        // The UI reads this state through the status endpoint.
        state.active = true;
        state.stop_tx = Some(stop_tx);
        state.task = Some(task);

        Ok(())
    }

    // This stops the monitor task and waits for it to finish.
    // It is safe to call even if monitoring is already stopped.
    // That makes the API easier to use from the UI.
    pub async fn stop(&self) {
        let mut state = self.state.lock().await;
        if !state.active {
            return;
        }

        // Send the stop signal if the channel still exists.
        // If the receiver has already gone away, the send will fail silently.
        // That is fine because the task is already finished.
        if let Some(stop_tx) = state.stop_tx.take() {
            let _ = stop_tx.send(());
        }

        // Wait for the task to exit.
        // This ensures the task has cleaned up before we return.
        // Awaiting here does not block the thread; it just waits asynchronously.
        if let Some(task) = state.task.take() {
            let _ = task.await;
        }

        // Reset peak to zero since monitoring has stopped.
        // This avoids showing a stale peak in the UI.
        // The atomic update is safe to do without extra locks.
        self.peak.store(0f32.to_bits(), Ordering::Relaxed);
        state.active = false;
    }

    // This returns the current monitoring status for the UI.
    // It reads the shared atomic and the active flag.
    // The UI can poll this to update the display.
    pub async fn status(&self) -> MonitorStatus {
        let state = self.state.lock().await;
        MonitorStatus {
            active: state.active,
            peak: f32::from_bits(self.peak.load(Ordering::Relaxed)),
        }
    }
}

// This task listens to the UDP stream and updates the peak value.
// It acts like a lightweight client that decodes audio locally.
// The loop ends when the stop signal is received.
async fn run_monitor(
    // `config` provides the audio format so we can decode packets correctly.
    // Using the config keeps the monitor in sync with the server.
    // It avoids hard‑coding sample rate or channel count.
    config: StreamConfig,
    // `peak` is the shared atomic that will store the latest peak level.
    // The task updates this value whenever a packet is decoded.
    // Other parts of the program can read it without locking.
    peak: Arc<AtomicU32>,
    // `stop_rx` is a one‑shot receiver used to shut down the task.
    // When a message arrives, the loop exits.
    // This is a clean way to coordinate shutdown.
    stop_rx: &mut oneshot::Receiver<()>,
) {
    // Bind a UDP socket on localhost using port 0 (the OS picks a free port).
    // Binding to localhost keeps the monitor traffic inside the same machine.
    // Port 0 avoids conflicts by letting the OS choose a free port.
    let socket = match UdpSocket::bind("127.0.0.1:0").await {
        Ok(socket) => socket,
        Err(_) => return,
    };
    // `server_addr` is the address of the local streaming server.
    // This is where the monitor sends its Hello and KeepAlive packets.
    // It uses the same UDP port as the real clients.
    let server_addr = SocketAddr::from(([127, 0, 0, 1], config.udp_port));
    // Choose mono or stereo decoding based on the stream configuration.
    // This ensures the decoder interprets the samples correctly.
    // The opus library requires this information.
    let channels = match config.channels {
        1 => Channels::Mono,
        _ => Channels::Stereo,
    };
    // Create the Opus decoder used to turn packets back into PCM.
    // If the decoder cannot be created, monitoring simply stops.
    // This is a safe failure mode for a non‑critical feature.
    let mut decoder = match Decoder::new(config.sample_rate, channels) {
        Ok(decoder) => decoder,
        Err(_) => return,
    };

    // Build a Hello packet so the server knows this client exists.
    // This registers the monitor as an active client for UDP streaming.
    // It mirrors the behavior of a real client.
    let hello = build_control_packet(MSG_HELLO);
    // Build a KeepAlive packet that will be sent repeatedly.
    // This prevents the server from timing out the monitor client.
    // The keepalive interval is set below with a timer.
    let keepalive = build_control_packet(MSG_KEEPALIVE);
    let _ = socket.send_to(&hello, server_addr).await;

    // Create a timer that ticks every two seconds for keepalive sending.
    // Tokio’s interval timer is async and does not block the thread.
    // This helps maintain a steady heartbeat to the server.
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    // `buffer` is where incoming UDP data is stored temporarily.
    // It is reused for each packet to avoid repeated allocations.
    // The size is large enough for typical stream packets.
    let mut buffer = vec![0u8; 4096];
    // `pcm` is a reusable buffer for decoded samples.
    // It is sized for one frame of audio.
    // Reusing it avoids allocating a new vector each time.
    let mut pcm = vec![0i16; config.frame_size * config.channels as usize];

    loop {
        tokio::select! {
            // If the stop signal arrives, exit the loop.
            // The `&mut *stop_rx` syntax allows the receiver to be polled multiple times.
            // This is a common async pattern in Rust.
            _ = &mut *stop_rx => {
                break;
            }
            // If the timer ticks, send a keepalive packet.
            // Keepalives are lightweight and help maintain the client list.
            // This keeps the server from removing the monitor client.
            _ = interval.tick() => {
                let _ = socket.send_to(&keepalive, server_addr).await;
            }
            // Otherwise wait for the next UDP packet to arrive.
            // The `recv_from` call yields until data is available.
            // This is efficient because it does not block the runtime thread.
            result = socket.recv_from(&mut buffer) => {
                let Ok((len, _addr)) = result else { continue; };
                if len < HEADER_SIZE {
                    continue;
                }

                // Ignore packets that are not the expected protocol version.
                // This protects us from unexpected or incompatible data.
                // The version check is cheap and helps prevent confusion.
                if buffer[0] != VERSION {
                    continue;
                }

                // Read the payload length from the header.
                // The header stores the length as a 16‑bit big‑endian value.
                // This allows the monitor to know how many bytes to decode.
                let payload_len = u16::from_be_bytes([buffer[13], buffer[14]]) as usize;
                if HEADER_SIZE + payload_len > len {
                    continue;
                }

                // Extract the Opus payload and decode it into PCM samples.
                // The decoder fills the `pcm` buffer with raw samples.
                // The `false` flag means “not using forward error correction.”
                let payload = &buffer[15..15 + payload_len];
                let Ok(samples) = decoder.decode(payload, &mut pcm, false) else { continue; };
                let sample_count = samples * config.channels as usize;

                // Compute the peak level by looking for the largest sample.
                // This is a simple way to approximate “loudness.”
                // It is not perfect, but it is fast and good enough for a meter.
                let mut max = 0.0f32;
                for &sample in pcm.iter().take(sample_count) {
                    let value = (sample as f32 / i16::MAX as f32).abs();
                    if value > max {
                        max = value;
                    }
                }
                peak.store(max.to_bits(), Ordering::Relaxed);
            }
        }
    }
}

// This builds a small control packet with the magic bytes and message type.
// The packet is very small and fits in a tiny vector.
// It is used for Hello and KeepAlive messages.
fn build_control_packet(msg_type: u8) -> Vec<u8> {
    // `buf` is a byte vector sized for the magic bytes plus two extra fields.
    // Pre‑allocating the capacity avoids resizing during pushes.
    // The data is stored on the heap because `Vec` uses heap storage.
    let mut buf = Vec::with_capacity(6);
    buf.extend_from_slice(&MAGIC);
    buf.push(VERSION);
    buf.push(msg_type);
    buf
}

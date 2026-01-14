use crate::audio::capture::PcmFrame;
use crate::audio::controller::AudioSource;
use crate::control::messages::{StreamConfig, build_stream_packet};
use crate::encode::opus::OpusEncoder;
use crate::transport::udp::UdpServer;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot, watch};
use tokio::task::JoinHandle;

// `StartStreamRequest` describes the JSON body sent by the UI when a user
// clicks the “start stream” button.
// `Deserialize` tells Rust how to turn JSON into this struct automatically.
// That means we don’t need to parse the JSON manually.
#[derive(Deserialize)]
pub struct StartStreamRequest {
    // `sink` is the name of the PulseAudio sink to capture from.
    // It is a plain string because that is easy to pass from the UI.
    // Strings in Rust are UTF‑8 by default.
    pub sink: String,
}

// `StreamStatusResponse` is the JSON object returned to the UI when it asks
// whether streaming is running.
// `Serialize` tells Rust how to turn this struct into JSON.
// That makes HTTP handlers very simple to write.
#[derive(Serialize)]
pub struct StreamStatusResponse {
    // `running` tells the UI if streaming is currently active.
    // A boolean is either true or false, which fits this state well.
    // This makes UI logic simple.
    pub running: bool,
    // `sink` holds the current sink name, or `None` if not running.
    // `Option` is Rust’s safe “maybe” type for values that may be missing.
    // It prevents accidental null pointer errors.
    pub sink: Option<String>,
    // `udp_port` tells the UI which UDP port clients should use.
    // This is useful for external clients that connect directly.
    // The port is part of the protocol configuration.
    pub udp_port: u16,
}

// `StreamManager` is the coordinator for starting, stopping, and running the
// audio streaming loop.
// It glues together audio capture, encoding, and UDP sending.
// Keeping this in one place makes the flow easier to follow.
pub struct StreamManager {
    // `audio` is a shared handle to something that can start and stop capture.
    // The `Arc` lets multiple tasks share the same object safely.
    // The `dyn AudioSource` part means it is a trait object.
    audio: Arc<dyn AudioSource>,
    // `udp` is the shared UDP server used to send packets to clients.
    // This is also in an `Arc` so the streaming task can own a clone.
    // Sharing avoids the need to recreate sockets.
    udp: Arc<UdpServer>,
    // `config` holds stream settings like sample rate and frame size.
    // The manager keeps a copy so it always knows how to encode.
    // `StreamConfig` is small, so copying is cheap.
    config: StreamConfig,
    // `state` stores runtime information and is guarded by a mutex for safety.
    // A mutex ensures only one task can update the state at a time.
    // This avoids races between start/stop requests.
    state: Mutex<StreamRuntime>,
    // `shutdown_tx` is used to broadcast a shutdown signal to the web server.
    // A watch channel always holds the latest value, so listeners can catch up.
    // This is an easy way to tell multiple tasks to stop.
    shutdown_tx: watch::Sender<bool>,
}

// `StreamRuntime` is the internal state for the currently running stream.
// It is kept separate so the mutex only guards this small set of fields.
// This makes locking quicker and reduces complexity.
struct StreamRuntime {
    // `running` is true when the streaming loop is active.
    // This prevents starting a second stream by accident.
    // It is a simple and effective guard.
    running: bool,
    // `sink` stores the current sink name if streaming is active.
    // Storing it here lets the status endpoint report it to the UI.
    // `Option` is used because there might be no active sink.
    sink: Option<String>,
    // `stop_tx` is a one‑shot channel used to stop the streaming task.
    // A oneshot channel is a “fire once” signal.
    // It is good for stop requests because only one signal is needed.
    stop_tx: Option<oneshot::Sender<()>>,
    // `task` holds the handle to the background streaming task.
    // The handle allows the manager to await or abort the task.
    // Tasks are lightweight units of async work.
    task: Option<JoinHandle<()>>,
}

impl StreamManager {
    // This creates a new manager. The arguments are shared handles so multiple
    // tasks can access the same stream manager safely.
    // Rust encourages sharing immutable data and locking only when needed.
    // This setup follows that pattern.
    pub fn new(
        audio: Arc<dyn AudioSource>,
        udp: Arc<UdpServer>,
        config: StreamConfig,
        shutdown_tx: watch::Sender<bool>,
    ) -> Self {
        Self {
            audio,
            udp,
            config,
            state: Mutex::new(StreamRuntime {
                running: false,
                sink: None,
                stop_tx: None,
                task: None,
            }),
            shutdown_tx,
        }
    }

    // This starts audio capture and launches the streaming loop in the background.
    // It returns an error if a stream is already running.
    // The method is async so it can wait for the audio thread.
    pub async fn start(&self, sink: String) -> Result<(), String> {
        // `state` is locked so we can safely update runtime state.
        // Locking returns a guard that releases automatically when it goes out of scope.
        // This prevents forgetting to unlock.
        let mut state = self.state.lock().await;
        // If already running, return an error so the caller can show a message.
        // Returning a `String` makes it easy for the UI to display the reason.
        // This is a simple error‑reporting approach for user‑facing endpoints.
        if state.running {
            return Err("Stream already running".to_string());
        }

        // `receiver` is an async channel that yields PCM frames from the audio thread.
        // The `.await` means this function pauses until capture actually starts.
        // This gives the caller confidence that streaming has begun.
        let receiver = self.audio.start_capture(sink.clone(), self.config).await?;
        // `encoder` converts PCM samples into Opus packets for streaming.
        // If the encoder fails to create, capture is stopped so resources are not leaked.
        // This is a good example of cleanup on error.
        let mut encoder = match OpusEncoder::new(self.config) {
            Ok(encoder) => encoder,
            Err(err) => {
                let _ = self.audio.stop_capture().await;
                return Err(err.to_string());
            }
        };
        // `udp` is cloned so the background task can own a handle.
        // This is a cheap clone because `Arc` only increments a counter.
        // It does not duplicate the underlying socket.
        let udp = Arc::clone(&self.udp);
        // `stop_tx` and `stop_rx` are used to tell the background task to stop.
        // The sender is kept in state, while the receiver is owned by the task.
        // This is a clean way to signal task shutdown.
        let (stop_tx, mut stop_rx) = oneshot::channel();

        // This spawns the streaming loop as a background async task.
        // The `async move` block moves the captured variables into the task.
        // Spawning keeps the UI responsive while streaming continues.
        let task = tokio::spawn(async move {
            stream_loop(&mut encoder, receiver, udp, &mut stop_rx).await;
        });

        // Update state so the UI can see that streaming is active.
        // These assignments happen while the mutex is held, so they are safe.
        // Keeping state consistent avoids confusing UI results.
        state.running = true;
        state.sink = Some(sink);
        state.stop_tx = Some(stop_tx);
        state.task = Some(task);

        Ok(())
    }

    // This stops the streaming loop and shuts down the audio capture.
    // It waits briefly for the background task to finish.
    // If the task does not stop, it is aborted to avoid hanging.
    pub async fn stop(&self) -> Result<(), String> {
        // `state` is locked to safely update runtime information.
        // The lock ensures start/stop requests do not interleave improperly.
        // This is a key safety feature in concurrent programs.
        let mut state = self.state.lock().await;
        // If nothing is running, there is nothing to stop.
        // Returning `Ok(())` keeps the API simple for callers.
        // This makes stop idempotent, which is often desirable.
        if !state.running {
            return Ok(());
        }

        // If a stop channel exists, send the stop signal to the streaming task.
        // If the receiver is gone, sending will fail silently.
        // That is acceptable because the task is already finished.
        if let Some(stop_tx) = state.stop_tx.take() {
            let _ = stop_tx.send(());
        }

        // Tell the audio thread to stop capturing.
        // This ensures no more PCM frames are produced.
        // The `await` yields while the audio thread processes the stop.
        self.audio.stop_capture().await?;

        // Wait briefly for the background task to finish, then abort if it hangs.
        // The timeout prevents the server from being stuck forever on shutdown.
        // Aborting is a last resort, but it keeps the program responsive.
        if let Some(task) = state.task.take() {
            let mut task = task;
            match tokio::time::timeout(std::time::Duration::from_secs(2), &mut task).await {
                Ok(_) => {}
                Err(_) => {
                    task.abort();
                }
            }
        }

        // Reset the state so streaming can be started again later.
        // Clearing the sink and running flag keeps status accurate.
        // This also releases the stop channel and task handle.
        state.running = false;
        state.sink = None;

        Ok(())
    }

    // This returns a snapshot of the current streaming status.
    // The UI uses this to show “running” or “stopped.”
    // The status is a lightweight copy of the internal state.
    pub async fn status(&self) -> StreamStatusResponse {
        let state = self.state.lock().await;
        StreamStatusResponse {
            running: state.running,
            sink: state.sink.clone(),
            udp_port: self.config.udp_port,
        }
    }

    // This sends a shutdown signal to the rest of the system.
    // It does not stop streaming directly; it just triggers watchers.
    // Using a watch channel is a simple broadcast mechanism.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

// This loop waits for PCM frames, encodes them, and sends them via UDP.
// It is designed to run as a background task.
// The loop exits when it receives a stop signal or the receiver closes.
async fn stream_loop(
    // `encoder` is reused for each frame to avoid reallocation.
    // A mutable reference allows the encoder to update its internal state.
    // The lifetime of the encoder is tied to this task.
    encoder: &mut OpusEncoder,
    // `receiver` yields PCM frames from the audio thread.
    // Each `recv` call waits asynchronously for the next frame.
    // When the channel closes, `recv` returns `None`.
    mut receiver: tokio::sync::mpsc::Receiver<PcmFrame>,
    // `udp` is used to send packets to all active clients.
    // It is wrapped in `Arc` so the task can hold a clone.
    // This avoids ownership conflicts with other parts of the program.
    udp: Arc<UdpServer>,
    // `stop_rx` provides a signal to stop the loop.
    // A oneshot receiver yields once and then completes.
    // This is a clean and lightweight stop mechanism.
    stop_rx: &mut oneshot::Receiver<()>,
) {
    // `seq` is the sequence number added to each outgoing packet.
    // It helps clients detect missing or out‑of‑order packets.
    // The `u32` range is large enough for long streams.
    let mut seq = 0u32;

    loop {
        // `tokio::select!` lets the loop respond to whichever event happens first.
        // It is similar to “wait on multiple things at once.”
        // This avoids writing complicated manual polling code.
        tokio::select! {
            // Stop immediately if a stop signal is received.
            // The `&mut *stop_rx` syntax allows the receiver to be polled repeatedly.
            // It is a small Rust pattern you often see in async loops.
            _ = &mut *stop_rx => {
                break;
            }
            // Otherwise wait for the next audio frame from the receiver.
            // This call yields while waiting, keeping the runtime responsive.
            // If the channel is closed, the loop exits.
            frame = receiver.recv() => {
                let Some(frame) = frame else { break; };
                // Encode PCM into Opus; if encoding fails, skip this frame.
                // Skipping is acceptable because audio is a continuous stream.
                // It is better to drop one frame than stop the whole stream.
                let Ok(opus) = encoder.encode_frame(&frame.samples) else { continue; };
                // Build the UDP packet including sequence number and timestamp.
                // This packs all metadata and audio payload into a byte buffer.
                // The client expects this exact format.
                let packet = build_stream_packet(seq, frame.timestamp_ms, &opus);
                // Send the packet to all registered clients.
                // Because this is UDP, a send error usually just means the client is gone.
                // The server keeps going regardless.
                udp.send_to_clients(&packet).await;
                // Increase the sequence number, wrapping at the end of the range.
                // `wrapping_add` avoids overflow panics by looping back to zero.
                // This is safe because sequence numbers are only relative.
                seq = seq.wrapping_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::messages::DEFAULT_STREAM_CONFIG;
    use async_trait::async_trait;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex as AsyncMutex;

    // `MockAudio` is a fake audio source used to test streaming logic.
    // It counts how many times start/stop are called.
    // It also provides a way to inject fake frames.
    struct MockAudio {
        // `start_calls` counts how many times `start_capture` was called.
        // Atomic counters can be safely updated from multiple threads.
        // We use `AtomicUsize` so the count fits the platform size.
        start_calls: AtomicUsize,
        // `stop_calls` counts how many times `stop_capture` was called.
        // Using atomics avoids needing a mutex in the test.
        // The ordering is `SeqCst` for maximum simplicity.
        stop_calls: AtomicUsize,
        // `sender` stores a channel used to inject frames into the stream loop.
        // It is wrapped in an async mutex because it is accessed across await points.
        // This keeps the test safe from data races.
        sender: AsyncMutex<Option<tokio::sync::mpsc::Sender<PcmFrame>>>,
    }

    impl MockAudio {
        // This creates a new mock audio source with counters set to zero.
        // Tests often use small helper constructors like this.
        // It keeps each test short and readable.
        fn new() -> Self {
            Self {
                start_calls: AtomicUsize::new(0),
                stop_calls: AtomicUsize::new(0),
                sender: AsyncMutex::new(None),
            }
        }

        // This helper sends a dummy frame into the stream loop.
        // It simulates one chunk of audio being captured.
        // Using zeros is fine for tests; we just need any data.
        async fn push_frame(&self) {
            let frame = PcmFrame {
                timestamp_ms: 1,
                samples: vec![
                    0i16;
                    DEFAULT_STREAM_CONFIG.frame_size
                        * DEFAULT_STREAM_CONFIG.channels as usize
                ],
            };

            if let Some(sender) = self.sender.lock().await.as_ref() {
                let _ = sender.send(frame).await;
            }
        }
    }

    #[async_trait]
    impl AudioSource for MockAudio {
        async fn start_capture(
            &self,
            _sink: String,
            _config: StreamConfig,
        ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, String> {
            self.start_calls.fetch_add(1, Ordering::SeqCst);
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            *self.sender.lock().await = Some(tx);
            Ok(rx)
        }

        async fn stop_capture(&self) -> Result<(), String> {
            self.stop_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn stream_manager_start_stop() {
        let config = DEFAULT_STREAM_CONFIG;
        let audio = Arc::new(MockAudio::new());
        let audio_source: Arc<dyn AudioSource> = audio.clone();
        let udp = Arc::new(
            UdpServer::bind(SocketAddr::from(([127, 0, 0, 1], 0)), config)
                .await
                .unwrap(),
        );
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let manager = StreamManager::new(audio_source, udp, config, shutdown_tx);

        let status = manager.status().await;
        assert!(!status.running);

        manager.start("sink".to_string()).await.unwrap();
        let status = manager.status().await;
        assert!(status.running);

        audio.push_frame().await;

        manager.stop().await.unwrap();
        let status = manager.status().await;
        assert!(!status.running);

        assert_eq!(audio.start_calls.load(Ordering::SeqCst), 1);
        assert_eq!(audio.stop_calls.load(Ordering::SeqCst), 1);
    }
}

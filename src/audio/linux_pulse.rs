use crate::audio::capture::{CaptureSession, PcmFrame, PulseStreamHandle, RingBuffer};
use crate::control::messages::StreamConfig;
use libpulse_binding as pulse;
use pulse::context::{Context, FlagSet, State};
use pulse::def::BufferAttr;
use pulse::mainloop::threaded::Mainloop;
use pulse::sample::Format;
use pulse::stream::{FlagSet as StreamFlagSet, Stream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

// `SinkMeter` holds the information needed to track peak levels for a sink.
// Each meter has a name, a shared peak value, and an optional stream handle.
// The stream handle is optional because it may be shut down during cleanup.
struct SinkMeter {
    // `name` is the sink name as a String.
    // This label is used in the UI so users can identify the meter.
    // Strings are owned here to keep the name alive for the meter’s lifetime.
    name: String,
    // `peak` is a shared atomic `u32` that stores a floating‑point value as bits.
    // We store floats as bits because atomic floats are not built into Rust.
    // The UI converts these bits back into `f32` when needed.
    peak: Arc<AtomicU32>,
    // `stream` owns the PulseAudio stream that feeds this meter.
    // It is wrapped in `Option` so it can be taken and shut down safely.
    // This makes cleanup logic simple and explicit.
    stream: Option<PulseStreamHandle>,
}

// `PulseManager` owns the PulseAudio connection, mainloop, and monitoring streams.
// It is the central place where PulseAudio is managed.
// Keeping it in one struct keeps the rest of the program simpler.
pub struct PulseManager {
    // `mainloop` is the threaded PulseAudio mainloop. It runs callbacks on its own
    // internal thread, which is why we lock and unlock it when touching the context.
    // The lock ensures PulseAudio’s internal state is not modified concurrently.
    // This is required by the PulseAudio API.
    mainloop: Mainloop,
    // `context` represents the PulseAudio connection for this client.
    // Think of it as the “session” with the audio server.
    // All streams are created from this context.
    context: Context,
    // `meters` stores the active sink monitor streams and their shared peak values.
    // The vector grows to match the number of sinks present on the system.
    // This list is used by the UI to show live levels.
    meters: Vec<SinkMeter>,
}

impl PulseManager {
    // This creates the mainloop and context, starts the thread, and connects.
    // If any step fails, an error is returned to the caller.
    // This keeps setup failures explicit instead of silently ignored.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        // `mainloop` is created first; it drives PulseAudio callbacks.
        // The `ok_or` turns a `None` into an error for easier handling.
        // This is a common Rust pattern for fallible creation.
        let mut mainloop = Mainloop::new().ok_or("Failed to create PulseAudio mainloop")?;
        // `context` is the connection handle to PulseAudio with a friendly name.
        // The name can show up in PulseAudio tools, which helps debugging.
        // The context is created using the mainloop.
        let mut context =
            Context::new(&mainloop, "MinntyServer").ok_or("Failed to create PulseAudio context")?;

        // This starts the mainloop thread so it can process events and callbacks.
        // Without this call, no callbacks would run and the connection would stall.
        // It is a required step for threaded PulseAudio loops.
        mainloop.start()?;

        // The context must be used while the mainloop is locked.
        // This is a safety requirement from the PulseAudio API.
        // Locking ensures exclusive access while changing connection state.
        mainloop.lock();
        context.connect(None, FlagSet::NOFLAGS, None)?;
        mainloop.unlock();

        // `manager` collects the mainloop and context together in one struct.
        // This makes it easier to pass them around as a single unit.
        // The meters list starts empty and is filled later.
        let mut manager = Self {
            mainloop,
            context,
            meters: Vec::new(),
        };

        // Wait until the connection is ready before returning.
        // This avoids later code trying to use an unready context.
        // It makes startup failures happen early and clearly.
        manager.wait_for_ready()?;
        Ok(manager)
    }

    // This waits for the PulseAudio connection to become ready or fail.
    // It polls the context state until it reaches Ready or times out.
    // This ensures callers only proceed when the connection is usable.
    fn wait_for_ready(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // `start` tracks when we began waiting.
        // We use this with `elapsed()` to implement a timeout.
        // Timeouts prevent endless waiting in case of failure.
        let start = Instant::now();
        // `timeout` sets how long we are willing to wait.
        // Five seconds is a reasonable default for local connections.
        // This can be tuned if needed.
        let timeout = Duration::from_secs(5);

        loop {
            self.mainloop.lock();
            let state = self.context.get_state();
            self.mainloop.unlock();

            match state {
                State::Ready => return Ok(()),
                State::Failed | State::Terminated => {
                    return Err("PulseAudio connection failed".into());
                }
                _ => {
                    if start.elapsed() > timeout {
                        return Err("PulseAudio connection timed out".into());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }

    // This asks PulseAudio for the list of sinks and sets up a meter for each.
    // It uses PulseAudio’s introspection API which returns results asynchronously.
    // The function then waits for those results on a standard channel.
    pub fn start_meters(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        use pulse::callbacks::ListResult;

        // `Msg` is used to pass sink names from the callback to this function.
        // This is an internal helper so we can interpret callback results.
        // It is scoped here because only this function needs it.
        enum Msg {
            Item(String),
            End,
            Error,
        }

        // `tx` and `rx` are a blocking channel used for callback results.
        // The callback runs on the PulseAudio thread, so this channel bridges threads.
        // Using a channel keeps the code simple and avoids shared mutable state.
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();

        // The introspection call must be made while the mainloop is locked.
        // This registers a callback that PulseAudio will call with sink info.
        // The callback sends each item through the channel.
        self.mainloop.lock();
        self.context
            .introspect()
            .get_sink_info_list(move |res| match res {
                ListResult::Item(i) => {
                    if let Some(name) = i.name.as_ref() {
                        let _ = tx.send(Msg::Item(name.to_string()));
                    }
                }
                ListResult::End => {
                    let _ = tx.send(Msg::End);
                }
                ListResult::Error => {
                    let _ = tx.send(Msg::Error);
                }
            });
        self.mainloop.unlock();

        // Drain the channel until we get End or Error.
        // The timeout prevents waiting forever if the callback never finishes.
        // Each received item becomes a new meter stream.
        while let Ok(msg) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
            match msg {
                Msg::Item(name) => {
                    // `peak` is a shared atomic storing the current peak value.
                    // It starts at 0.0, encoded as raw bits.
                    // The UI will read this atomically without locking.
                    let peak = Arc::new(AtomicU32::new(0f32.to_bits()));
                    // `stream` is the monitor stream for this sink.
                    // It is created by connecting to the sink’s monitor source.
                    // The handle is stored so it can be shut down later.
                    let stream = self.start_sink_monitor(&name, peak.clone())?;
                    self.meters.push(SinkMeter {
                        name,
                        peak,
                        stream: Some(stream),
                    });
                }
                Msg::End => break,
                Msg::Error => return Err("PulseAudio sink enumeration failed".into()),
            }
        }

        Ok(())
    }

    // This starts a monitor stream for one sink and updates the shared peak value.
    // The monitor stream is separate from the main capture stream.
    // It listens to the sink’s monitor source in PulseAudio.
    fn start_sink_monitor(
        &mut self,
        sink_name: &str,
        peak: Arc<AtomicU32>,
    ) -> Result<PulseStreamHandle, Box<dyn std::error::Error>> {
        // `spec` defines the sample format for meter data.
        // We use 16‑bit signed little‑endian samples at 44.1kHz.
        // This format is common and easy to process.
        let spec = pulse::sample::Spec {
            format: Format::S16le,
            rate: 44100,
            channels: 2,
        };

        // `buffer_attr` configures how PulseAudio buffers meter samples.
        // These values are large to avoid dropping samples in the meter stream.
        // Metering is low bandwidth, so large buffers are acceptable.
        let buffer_attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: 1024,
        };

        self.mainloop.lock();

        // `stream` is the PulseAudio stream that will receive monitor data.
        // The stream is created with a name so it appears in PulseAudio tools.
        // If creation fails, an error is returned.
        let stream =
            Stream::new(&mut self.context, "meter", &spec, None).ok_or("Stream creation failed")?;
        // `handle` wraps the stream so it can be shut down later.
        // The handle remembers the mainloop pointer for locking on shutdown.
        // This keeps cleanup logic centralized in one type.
        let mut handle = PulseStreamHandle::new(stream, &self.mainloop);
        // `stream_ptr` is a raw pointer needed by PulseAudio callbacks.
        // The C API expects raw pointers, so Rust has to use `unsafe` here.
        // We are careful to keep the stream alive while the pointer is used.
        let stream_ptr = handle.as_ptr();

        unsafe {
            // This registers a callback to compute peak values when data arrives.
            // The callback runs on the PulseAudio mainloop thread.
            // It should do minimal work to keep audio processing smooth.
            (*stream_ptr).set_read_callback(Some(Box::new(move |_n| {
                let s = &mut *stream_ptr;

                while let Ok(pulse::stream::PeekResult::Data(data)) = s.peek() {
                    let samples =
                        std::slice::from_raw_parts(data.as_ptr() as *const i16, data.len() / 2);

                    let mut max = 0.0f32;
                    for &v in samples {
                        let f = (v as f32 / i16::MAX as f32).abs();
                        if f > max {
                            max = f;
                        }
                    }

                    peak.store(max.to_bits(), Ordering::Relaxed);
                    let _ = s.discard();
                }
            })));

            // This connects the stream to the sink's monitor source.
            // The `connect_record` call tells PulseAudio to feed us audio data.
            // The `ADJUST_LATENCY` flag lets PulseAudio tune buffering automatically.
            (*stream_ptr).connect_record(
                Some(&format!("{}.monitor", sink_name)),
                Some(&buffer_attr),
                StreamFlagSet::ADJUST_LATENCY,
            )?;
        }

        self.mainloop.unlock();
        Ok(handle)
    }

    // This starts a capture stream and returns a session plus a frame receiver.
    // The receiver is used by async tasks to pull PCM frames.
    // The session handle keeps the stream alive on the audio thread.
    pub fn start_capture(
        &mut self,
        sink_name: &str,
        config: StreamConfig,
    ) -> Result<(CaptureSession, mpsc::Receiver<PcmFrame>), Box<dyn std::error::Error>> {
        // `spec` describes the audio format to capture, based on `config`.
        // These values must match what the encoder expects later.
        // PulseAudio will deliver samples in this exact format.
        let spec = pulse::sample::Spec {
            format: Format::S16le,
            rate: config.sample_rate,
            channels: config.channels,
        };

        // `frame_samples` is the number of samples in one frame (all channels).
        // It is the per‑channel frame size times the channel count.
        // This is used for building complete frames from raw data.
        let frame_samples = config.frame_size * config.channels as usize;
        // `frag_bytes` is the fragment size in bytes for PulseAudio buffering.
        // PulseAudio uses this to determine how much data to deliver at a time.
        // Each sample is 2 bytes because it is i16.
        let frag_bytes = (frame_samples * 2) as u32;

        // `buffer_attr` configures how PulseAudio buffers capture data.
        // The large values help prevent underruns while still controlling fragment size.
        // This gives a balance between latency and stability.
        let buffer_attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: frag_bytes,
        };

        // `tx` and `rx` are an async channel used to send PCM frames to Tokio tasks.
        // The queue depth controls how many frames can be buffered.
        // This helps smooth out small bursts of processing delay.
        let (tx, rx) = mpsc::channel(config.pcm_queue_depth);
        let tx = tx.clone();

        self.mainloop.lock();

        // `stream` is the PulseAudio capture stream for the selected sink.
        // If the stream cannot be created, we return an error immediately.
        // Stream creation is the point where PulseAudio resources are allocated.
        let stream = Stream::new(&mut self.context, "capture", &spec, None)
            .ok_or("Capture stream creation failed")?;
        // `handle` wraps the stream and mainloop pointer.
        // The handle lets us shut down the stream cleanly later.
        // It also provides the raw pointer used in callbacks.
        let mut handle = PulseStreamHandle::new(stream, &self.mainloop);
        // `stream_ptr` is a raw pointer required by the callback API.
        // Raw pointers are unsafe, so they are only used inside `unsafe` blocks.
        // We keep ownership of the stream to ensure the pointer remains valid.
        let stream_ptr = handle.as_ptr();
        // `ring` is a ring buffer that assembles frames from raw samples.
        // This allows us to build fixed‑size frames even if PulseAudio delivers
        // data in uneven chunks.
        let mut ring = RingBuffer::new(frame_samples * 16);

        unsafe {
            // This callback is called whenever PulseAudio has new data ready.
            // It runs on the PulseAudio thread, so it should be quick and simple.
            // Heavy processing is avoided to keep audio smooth.
            (*stream_ptr).set_read_callback(Some(Box::new(move |_n| {
                let s = &mut *stream_ptr;

                while let Ok(pulse::stream::PeekResult::Data(data)) = s.peek() {
                    let samples =
                        std::slice::from_raw_parts(data.as_ptr() as *const i16, data.len() / 2);
                    ring.push_samples(samples);
                    let _ = s.discard();

                    while let Some(frame_samples_vec) = ring.pop_frame(frame_samples) {
                        let timestamp_ms = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        let frame = PcmFrame {
                            timestamp_ms,
                            samples: frame_samples_vec,
                        };

                        match tx.try_send(frame) {
                            Ok(_) => {}
                            Err(TrySendError::Full(_)) => {
                                // If the queue is full, the frame is dropped to avoid
                                // unlimited memory growth.
                                // Dropping frames is acceptable in real‑time audio.
                                // It is better to drop than to fall behind indefinitely.
                            }
                            Err(TrySendError::Closed(_)) => return,
                        }
                    }
                }
            })));

            // This connects the capture stream to the sink's monitor source.
            // The monitor source represents what is being played on the sink.
            // This is how the server captures system audio output.
            (*stream_ptr).connect_record(
                Some(&format!("{}.monitor", sink_name)),
                Some(&buffer_attr),
                StreamFlagSet::ADJUST_LATENCY,
            )?;
        }

        self.mainloop.unlock();
        Ok((CaptureSession::new(handle), rx))
    }

    // This stops an active capture session by shutting down its stream.
    // The session is passed by value so this function takes ownership.
    // This ensures no other code can use the session after stopping.
    pub fn stop_capture(&mut self, mut capture: CaptureSession) {
        capture.shutdown();
    }

    // This stops and cleans up all meter streams.
    // It is called during shutdown to release resources.
    // Cleaning up here avoids leaks in PulseAudio.
    fn cleanup_meters(&mut self) {
        for meter in &mut self.meters {
            if let Some(handle) = meter.stream.as_mut() {
                handle.shutdown();
            }
        }
    }

    // This returns a list of meter handles so other modules can read peak values.
    // It clones the `Arc`s so callers get their own shared handles.
    // Cloning an `Arc` is cheap because it only increments a counter.
    pub fn export_meters(&self) -> Vec<(String, Arc<AtomicU32>)> {
        self.meters
            .iter()
            .map(|m| (m.name.clone(), Arc::clone(&m.peak)))
            .collect()
    }
}

// This ensures meter streams are shut down when the meter is dropped.
// Rust automatically calls `drop` when a value goes out of scope.
// This is a safety net for resource cleanup.
impl Drop for SinkMeter {
    fn drop(&mut self) {
        if let Some(handle) = self.stream.as_mut() {
            handle.shutdown();
        }
    }
}

// This ensures PulseAudio resources are released when the manager is dropped.
// The cleanup here mirrors the setup done in `new`.
// This is important because PulseAudio expects clean disconnects.
impl Drop for PulseManager {
    fn drop(&mut self) {
        self.cleanup_meters();
        self.mainloop.lock();
        self.context.disconnect();
        self.mainloop.unlock();
        self.mainloop.stop();
    }
}

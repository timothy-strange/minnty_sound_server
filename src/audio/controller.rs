use crate::audio::capture::{CaptureSession, PcmFrame};
use crate::audio::linux_pulse::PulseManager;
use crate::control::messages::StreamConfig;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::thread;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

// `MeterExport` is just a friendly name for a list of meter values. Each entry
// contains the sink name (`String`) and a shared atomic value (`Arc<AtomicU32>`)
// that stores the latest peak level in a thread‑safe way.
// A type alias like this is not a new type; it is just a clearer label for the
// same underlying type. That makes signatures easier to read for beginners.
pub type MeterExport = Vec<(String, Arc<AtomicU32>)>;

// `AudioController` is the front‑door object used by the async parts of the
// program. It does not capture audio itself; instead, it sends commands to a
// dedicated audio thread that owns the PulseAudio connection.
// Keeping this responsibility separate makes the program easier to reason about.
// It also avoids mixing blocking audio code with async network code.
pub struct AudioController {
    // `cmd_tx` has type `std::sync::mpsc::Sender<AudioCommand>`. This is the
    // sending end of a blocking channel used to send commands to the audio thread.
    // The audio thread holds the receiver end and waits for messages.
    // A “blocking channel” means the receiving thread can sleep until a message
    // arrives instead of spinning in a tight loop.
    // The `std::sync::mpsc` module is part of Rust’s standard library.
    cmd_tx: std::sync::mpsc::Sender<AudioCommand>,
}

// `AudioSource` is a trait (a shared behavior interface). It describes the two
// actions the rest of the program needs: start capture and stop capture. This
// is written as a trait so other implementations could be swapped in later.
// Traits in Rust are similar to interfaces in other languages.
// The `Send + Sync` bounds say this trait object can be shared across threads.
#[async_trait::async_trait]
pub trait AudioSource: Send + Sync {
    // This asks for capture to start. The `sink` string names the audio output
    // to monitor, and `config` supplies sample rate, channels, and frame size.
    // The result is an async receiver that yields `PcmFrame` values as they arrive.
    // The `async` keyword here means the caller can `await` the result without
    // blocking the thread.
    // Returning a `Result` is Rust’s way of saying “this might fail.”
    async fn start_capture(
        &self,
        sink: String,
        config: StreamConfig,
    ) -> Result<mpsc::Receiver<PcmFrame>, String>;
    // This asks the audio thread to stop capture, if it is currently active.
    // It returns `Ok(())` on success, which is a standard Rust success signal.
    // The empty tuple `()` means there is no extra data to return.
    async fn stop_capture(&self) -> Result<(), String>;
}

// `AudioCommand` is the set of messages the audio thread understands.
// Each variant can carry extra information needed to perform the action.
// Enums are a powerful Rust feature for representing “one of several choices.”
enum AudioCommand {
    // This variant requests capture to start and includes a reply channel.
    // The reply channel is needed because the audio thread owns the actual work.
    // Without a reply channel, the caller wouldn’t know if starting succeeded.
    StartCapture {
        // `sink` is the name of the PulseAudio sink to monitor.
        // Strings in Rust are UTF‑8 by default and live on the heap.
        sink: String,
        // `config` tells the audio thread how many channels, sample rate, etc.
        // Passing the config by value here is fine because it’s small and Copy.
        config: StreamConfig,
        // `response` is a one‑time channel used to send back success or error.
        // A oneshot channel is designed for a single reply.
        response: oneshot::Sender<Result<mpsc::Receiver<PcmFrame>, String>>,
    },
    // This variant requests capture to stop and includes an acknowledgement.
    // The acknowledgement lets the caller wait until cleanup is complete.
    // This makes the async API more predictable for the UI.
    StopCapture {
        // `response` is a one‑time channel used to confirm stopping is complete.
        // The receiver side will `await` this to know when it is safe to proceed.
        response: oneshot::Sender<()>,
    },
}

// This connects the `AudioController` to the `AudioSource` trait so it can be
// used by the rest of the program without exposing its internal thread logic.
// The implementation simply forwards calls to the controller methods.
// This pattern is common in Rust when you want a concrete type to satisfy a trait.
#[async_trait::async_trait]
impl AudioSource for AudioController {
    // This simply forwards the call to the controller's own method.
    // It keeps the trait implementation minimal and easy to read.
    // The compiler will inline this in many cases, so it has no performance cost.
    async fn start_capture(
        &self,
        sink: String,
        config: StreamConfig,
    ) -> Result<mpsc::Receiver<PcmFrame>, String> {
        self.start_capture(sink, config).await
    }

    // This simply forwards the call to the controller's own method.
    // Keeping this separate also makes the code easier to mock in tests.
    // It mirrors the method signature exactly so callers see no difference.
    async fn stop_capture(&self) -> Result<(), String> {
        self.stop_capture().await
    }
}

impl AudioController {
    // This constructor sets up the audio thread and returns the controller plus
    // the exported meter handles. If anything fails, it returns an error and the
    // program can decide how to handle it.
    // The error type is boxed so different error kinds can be returned uniformly.
    // This is a common Rust pattern for top‑level errors.
    pub fn new() -> Result<(Self, MeterExport), Box<dyn std::error::Error>> {
        // `cmd_tx` and `cmd_rx` form a standard blocking channel. The async side
        // uses the sender, and the audio thread uses the receiver.
        // `mpsc` means “multiple producer, single consumer.”
        // That means many parts of the program can send, but only one thread receives.
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        // `init_tx` and `init_rx` are another blocking channel used only during
        // startup so the audio thread can report success or failure.
        // This avoids racing ahead before audio is fully initialized.
        // A simple channel is enough because only one message is needed.
        let (init_tx, init_rx) = std::sync::mpsc::channel();

        // This spawns a dedicated OS thread. The audio thread is needed because
        // PulseAudio uses blocking APIs and internal callbacks.
        // The closure passed to `spawn` is executed on that new thread.
        // Threads in Rust are real OS threads, not green threads.
        thread::spawn(move || {
            // `pulse` has type `PulseManager`. It owns the PulseAudio context and
            // mainloop. If creation fails, send the error back and stop the thread.
            // The `match` here is Rust’s way of handling success vs failure explicitly.
            // It is similar to an if/else but for `Result` values.
            let mut pulse = match PulseManager::new() {
                Ok(pulse) => pulse,
                Err(err) => {
                    let _ = init_tx.send(Err(err.to_string()));
                    return;
                }
            };

            // This starts meter streams for each sink so peak levels can be read
            // by the UI. If it fails, report the error and stop.
            // Doing this early ensures the UI can show meters immediately.
            // The `if let Err` pattern is a compact way to handle only errors.
            if let Err(err) = pulse.start_meters() {
                let _ = init_tx.send(Err(err.to_string()));
                return;
            }

            // `meters` has type `MeterExport`, and holds the list of peak values.
            // This data is sent back so the UI can display real‑time levels.
            // The values are stored in atomics so other threads can update them safely.
            let meters = pulse.export_meters();
            // This tells the main thread that initialization succeeded.
            // The `Ok` value wraps the meters in a `Result`.
            let _ = init_tx.send(Ok(meters));

            // `capture` holds the currently active capture session, if any.
            // It is `Option` because there may be no active capture yet.
            // `Option` is Rust’s safe way to represent “maybe a value.”
            let mut capture: Option<CaptureSession> = None;

            // This loop blocks until a command arrives on `cmd_rx`.
            // The loop ends if the sender side is dropped.
            // This pattern is common for worker threads that react to commands.
            for cmd in cmd_rx {
                match cmd {
                    AudioCommand::StartCapture {
                        sink,
                        config,
                        response,
                    } => {
                        // If there is already a capture session, shut it down first.
                        // This avoids having two captures fighting over the same sink.
                        // It also releases resources before starting a new stream.
                        if let Some(active) = capture.take() {
                            pulse.stop_capture(active);
                        }

                        // `result` is a `Result` that will hold the receiver or an error.
                        // The inner `map` is used to save the session and return the receiver.
                        // This is a functional style that avoids repeating error handling.
                        let result = pulse
                            .start_capture(&sink, config)
                            .map(|(session, receiver)| {
                                capture = Some(session);
                                receiver
                            })
                            .map_err(|err| err.to_string());

                        // Send the result back to the async caller.
                        // If the caller dropped the channel, the send simply fails silently.
                        // That is okay because the caller is no longer waiting.
                        let _ = response.send(result);
                    }
                    AudioCommand::StopCapture { response } => {
                        // If a session is running, stop it now.
                        // The `take()` call replaces the option with `None` and returns the value.
                        // This ensures the handle is moved out safely.
                        if let Some(active) = capture.take() {
                            pulse.stop_capture(active);
                        }
                        // Send an acknowledgement back to the caller.
                        // Acknowledge messages are useful when cleanup must finish first.
                        // The caller can `await` this to know it is safe to continue.
                        let _ = response.send(());
                    }
                }
            }

            // If the command channel closes, stop any active capture session.
            // This is a safety net to make sure resources are released.
            // It will run when the program is shutting down.
            if let Some(active) = capture.take() {
                pulse.stop_capture(active);
            }
        });

        // This waits for the initialization reply from the audio thread.
        // `recv()` is blocking, meaning this thread waits until a message arrives.
        // Blocking here is okay because setup should not continue until audio is ready.
        let meters = init_rx
            .recv()
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Audio thread initialization failed",
                )
            })?
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))?;

        // Return the controller (which owns the sender) and the meter list.
        // The controller is lightweight; most heavy work remains on the audio thread.
        // Returning a tuple lets us return two values without defining a new struct.
        Ok((Self { cmd_tx }, meters))
    }

    // This sends a start command to the audio thread and waits for its reply.
    // The reply is either a receiver of frames or an error string.
    // The method is async so callers can await it without blocking threads.
    pub async fn start_capture(
        &self,
        sink: String,
        config: StreamConfig,
    ) -> Result<mpsc::Receiver<PcmFrame>, String> {
        // `response` and `rx` are a one‑time channel used for a single reply.
        // The sender is moved into the command and the receiver stays here.
        // This is a simple way to do a “request‑reply” conversation between threads.
        let (response, rx) = oneshot::channel();
        // This sends the command into the blocking channel. If sending fails, it
        // means the audio thread has already stopped.
        // The `map_err` converts the send error into a friendly string.
        // This keeps the error type consistent for callers.
        self.cmd_tx
            .send(AudioCommand::StartCapture {
                sink,
                config,
                response,
            })
            .map_err(|_| "Audio thread unavailable".to_string())?;

        // This waits for the audio thread to respond and returns the result.
        // The `.await` means this async function pauses until the reply arrives.
        // If the reply channel is dropped, an error string is returned instead.
        rx.await.map_err(|_| "Audio response dropped".to_string())?
    }

    // This sends a stop command to the audio thread and waits for acknowledgement.
    // The acknowledgement does not carry extra data; it just signals completion.
    // This ensures the caller doesn’t race ahead before capture has stopped.
    pub async fn stop_capture(&self) -> Result<(), String> {
        // `response` and `rx` are a one‑time channel for the stop acknowledgement.
        // The `oneshot` channel is perfect for single‑reply messages.
        // It avoids keeping unnecessary resources alive.
        let (response, rx) = oneshot::channel();
        // This sends the stop request to the audio thread.
        // If sending fails, it means the thread is gone.
        // The error message is kept simple for the UI layer.
        self.cmd_tx
            .send(AudioCommand::StopCapture { response })
            .map_err(|_| "Audio thread unavailable".to_string())?;
        // This waits for the acknowledgement and then returns success.
        // The acknowledgement’s value is ignored because it carries no extra info.
        // The `_` pattern means “I know this value exists but I don’t need it.”
        let _ = rx.await;
        Ok(())
    }
}

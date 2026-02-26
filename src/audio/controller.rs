use crate::audio::capture::{CaptureSession, PcmFrame};
use crate::audio::linux_pulse::PulseManager;
use crate::control::messages::StreamConfig;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::thread;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

pub type MeterExport = Vec<(String, Arc<AtomicU32>)>;

pub struct AudioController {
    cmd_tx: std::sync::mpsc::Sender<AudioCommand>,
}

#[async_trait::async_trait]
pub trait AudioSource: Send + Sync {
    async fn start_capture(
        &self,
        sink: String,
        config: StreamConfig,
    ) -> Result<mpsc::Receiver<PcmFrame>, String>;
    async fn stop_capture(&self) -> Result<(), String>;
}

enum AudioCommand {
    StartCapture {
        sink: String,
        config: StreamConfig,
        response: oneshot::Sender<Result<mpsc::Receiver<PcmFrame>, String>>,
    },
    StopCapture {
        response: oneshot::Sender<()>,
    },
}

#[async_trait::async_trait]
impl AudioSource for AudioController {
    async fn start_capture(
        &self,
        sink: String,
        config: StreamConfig,
    ) -> Result<mpsc::Receiver<PcmFrame>, String> {
        self.start_capture(sink, config).await
    }

    async fn stop_capture(&self) -> Result<(), String> {
        self.stop_capture().await
    }
}

impl AudioController {
    pub fn new() -> Result<(Self, MeterExport), Box<dyn std::error::Error>> {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (init_tx, init_rx) = std::sync::mpsc::channel();

        thread::spawn(move || {
            let mut pulse = match PulseManager::new() {
                Ok(pulse) => pulse,
                Err(err) => {
                    let _ = init_tx.send(Err(err.to_string()));
                    return;
                }
            };

            if let Err(err) = pulse.start_meters() {
                let _ = init_tx.send(Err(err.to_string()));
                return;
            }

            let meters = pulse.export_meters();
            let _ = init_tx.send(Ok(meters));

            let mut capture: Option<CaptureSession> = None;

            for cmd in cmd_rx {
                match cmd {
                    AudioCommand::StartCapture {
                        sink,
                        config,
                        response,
                    } => {
                        if let Some(active) = capture.take() {
                            pulse.stop_capture(active);
                        }

                        pulse.stop_meters();
                        crate::log_info!("audio: meters off for streaming");

                        let result = pulse
                            .start_capture(&sink, config)
                            .map(|(session, receiver)| {
                                capture = Some(session);
                                receiver
                            })
                            .map_err(|err| err.to_string());

                        if result.is_err() {
                            if let Err(err) = pulse.start_meters_if_stopped() {
                                crate::log_warn!("audio warning: meters restart failed after start error: {err}");
                            } else {
                                crate::log_info!("audio: meters restarted after start failure");
                            }
                        }

                        let _ = response.send(result);
                    }
                    AudioCommand::StopCapture { response } => {
                        if let Some(active) = capture.take() {
                            pulse.stop_capture(active);
                        }

                        std::thread::sleep(std::time::Duration::from_millis(100));
                        if let Err(err) = pulse.start_meters_if_stopped() {
                            crate::log_warn!("audio warning: meters restart failed after stop: {err}");
                        } else {
                            crate::log_info!("audio: meters restarted after stop");
                        }

                        let _ = response.send(());
                    }
                }
            }

            crate::log_warn!("audio warning: command loop exited unexpectedly");

            if let Some(active) = capture.take() {
                pulse.stop_capture(active);
            }
        });

        let meters = init_rx
            .recv()
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Audio thread initialization failed",
                )
            })?
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))?;

        Ok((Self { cmd_tx }, meters))
    }

    pub async fn start_capture(
        &self,
        sink: String,
        config: StreamConfig,
    ) -> Result<mpsc::Receiver<PcmFrame>, String> {
        let (response, rx) = oneshot::channel();
        self.cmd_tx
            .send(AudioCommand::StartCapture {
                sink,
                config,
                response,
            })
            .map_err(|_| "Audio thread unavailable".to_string())?;

        rx.await.map_err(|_| "Audio response dropped".to_string())?
    }

    pub async fn stop_capture(&self) -> Result<(), String> {
        let (response, rx) = oneshot::channel();
        self.cmd_tx
            .send(AudioCommand::StopCapture { response })
            .map_err(|_| "Audio thread unavailable".to_string())?;
        let _ = rx.await;
        Ok(())
    }
}

use crate::audio::capture::PcmFrame;
use crate::audio::controller::AudioSource;
use crate::control::messages::{StreamConfig, build_stream_packet};
use crate::encode::opus::OpusEncoder;
use crate::transport::udp::UdpServer;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot, watch};
use tokio::task::JoinHandle;

#[derive(Deserialize)]
pub struct StartStreamRequest {
    pub sink: String,
}

#[derive(Serialize)]
pub struct StreamStatusResponse {
    pub running: bool,
    pub sink: Option<String>,
    pub udp_port: u16,
}

pub struct StreamManager {
    audio: Arc<dyn AudioSource>,
    udp: Arc<UdpServer>,
    config: StreamConfig,
    state: Mutex<StreamRuntime>,
    shutdown_tx: watch::Sender<bool>,
}

struct StreamRuntime {
    running: bool,
    sink: Option<String>,
    stop_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl StreamManager {
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

    pub async fn start(&self, sink: String) -> Result<(), String> {
        let mut state = self.state.lock().await;
        if state.running {
            return Err("Stream already running".to_string());
        }

        let receiver = self.audio.start_capture(sink.clone(), self.config).await?;
        let mut encoder = match OpusEncoder::new(self.config) {
            Ok(encoder) => encoder,
            Err(err) => {
                let _ = self.audio.stop_capture().await;
                return Err(err.to_string());
            }
        };
        let udp = Arc::clone(&self.udp);
        let (stop_tx, mut stop_rx) = oneshot::channel();

        let task = tokio::spawn(async move {
            stream_loop(&mut encoder, receiver, udp, &mut stop_rx).await;
        });

        state.running = true;
        state.sink = Some(sink);
        state.stop_tx = Some(stop_tx);
        state.task = Some(task);

        Ok(())
    }

    pub async fn stop(&self) -> Result<(), String> {
        let mut state = self.state.lock().await;
        if !state.running {
            return Ok(());
        }

        if let Some(stop_tx) = state.stop_tx.take() {
            let _ = stop_tx.send(());
        }

        self.audio.stop_capture().await?;

        if let Some(task) = state.task.take() {
            let mut task = task;
            match tokio::time::timeout(std::time::Duration::from_secs(2), &mut task).await {
                Ok(_) => {}
                Err(_) => {
                    task.abort();
                }
            }
        }

        state.running = false;
        state.sink = None;

        Ok(())
    }

    pub async fn status(&self) -> StreamStatusResponse {
        let state = self.state.lock().await;
        StreamStatusResponse {
            running: state.running,
            sink: state.sink.clone(),
            udp_port: self.config.udp_port,
        }
    }

    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

async fn stream_loop(
    encoder: &mut OpusEncoder,
    mut receiver: tokio::sync::mpsc::Receiver<PcmFrame>,
    udp: Arc<UdpServer>,
    stop_rx: &mut oneshot::Receiver<()>,
) {
    let mut seq = 0u32;

    loop {
        tokio::select! {
            _ = &mut *stop_rx => {
                break;
            }
            frame = receiver.recv() => {
                let Some(frame) = frame else { break; };
                let Ok(opus) = encoder.encode_frame(&frame.samples) else { continue; };
                let packet = build_stream_packet(seq, frame.timestamp_ms, &opus);
                udp.send_to_clients(&packet).await;
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

    struct MockAudio {
        start_calls: AtomicUsize,
        stop_calls: AtomicUsize,
        sender: AsyncMutex<Option<tokio::sync::mpsc::Sender<PcmFrame>>>,
    }

    impl MockAudio {
        fn new() -> Self {
            Self {
                start_calls: AtomicUsize::new(0),
                stop_calls: AtomicUsize::new(0),
                sender: AsyncMutex::new(None),
            }
        }

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
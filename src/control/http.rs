use crate::audio::capture::PcmFrame;
use crate::audio::controller::AudioSource;
use crate::control::messages::{StreamConfig, build_stream_packet};
use crate::encode::opus::OpusEncoder;
use crate::testing::net_impairment::NetImpairmentController;
use crate::transport::udp::UdpServer;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

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
    impairment: Arc<NetImpairmentController>,
    state: Mutex<StreamRuntime>,
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
        impairment: Arc<NetImpairmentController>,
    ) -> Self {
        Self {
            audio,
            udp,
            config,
            impairment,
            state: Mutex::new(StreamRuntime {
                running: false,
                sink: None,
                stop_tx: None,
                task: None,
            }),
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
        let impairment = Arc::clone(&self.impairment);
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let frame_duration = Duration::from_secs_f64(
            self.config.frame_size as f64 / self.config.sample_rate as f64,
        );

        let task = tokio::spawn(async move {
            stream_loop(
                &mut encoder,
                receiver,
                udp,
                &mut stop_rx,
                frame_duration,
                impairment,
            )
            .await;
        });

        self.udp.set_streaming(true);
        state.running = true;
        state.sink = Some(sink);
        state.stop_tx = Some(stop_tx);
        state.task = Some(task);

        Ok(())
    }

    pub async fn stop(&self) -> Result<(), String> {
        let task = {
            let mut state = self.state.lock().await;
            if !state.running {
                return Ok(());
            }

            if let Some(stop_tx) = state.stop_tx.take() {
                let _ = stop_tx.send(());
            }

            state.running = false;
            state.sink = None;
            state.task.take()
        };

        if let Some(task) = task {
            let mut task = task;
            match tokio::time::timeout(std::time::Duration::from_secs(2), &mut task).await {
                Ok(_) => {}
                Err(_) => {
                    task.abort();
                }
            }
        }

        self.audio.stop_capture().await?;
        self.udp.set_streaming(false);

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

    #[cfg(feature = "net_impairment_ui")]
    pub fn trigger_test_gap_once_ms(&self, delay_ms: u64) -> Result<(), String> {
        self.impairment.trigger_gap_once_ms(delay_ms)
    }
}

async fn stream_loop(
    encoder: &mut OpusEncoder,
    mut receiver: tokio::sync::mpsc::Receiver<PcmFrame>,
    udp: Arc<UdpServer>,
    stop_rx: &mut oneshot::Receiver<()>,
    frame_duration: Duration,
    impairment: Arc<NetImpairmentController>,
) {
    let mut seq = 0u32;
    let mut next_send_at = Instant::now();
    let mut last_send_at: Option<Instant> = None;

    let mut sent_window = 0u64;
    let mut gap_over_200_window = 0u64;
    let mut burst_after_gap_window = 0u64;
    let mut send_delta_max_ms = 0.0f64;
    let mut pacer_late_max_ms = 0.0f64;
    let mut last_interval_was_gap = false;
    let mut summary_interval = tokio::time::interval(Duration::from_secs(2));

    loop {
        tokio::select! {
            _ = &mut *stop_rx => {
                break;
            }
            _ = summary_interval.tick() => {
                crate::log_info!(
                    "stream diag: sent={} gapOver200={} burstAfterGap={} sendDeltaMaxMs={:.2} pacerLateMsMax={:.2}",
                    sent_window,
                    gap_over_200_window,
                    burst_after_gap_window,
                    send_delta_max_ms,
                    pacer_late_max_ms
                );
                sent_window = 0;
                gap_over_200_window = 0;
                burst_after_gap_window = 0;
                send_delta_max_ms = 0.0;
                pacer_late_max_ms = 0.0;
            }
            frame = receiver.recv() => {
                let Some(frame) = frame else { break; };

                if let Some(gap) = impairment.take_gap_once() {
                    crate::log_warn!(
                        "stream test: injecting one-shot gap {} ms",
                        gap.as_millis()
                    );
                    let pause_deadline = Instant::now() + gap;
                    let mut dropped_during_gap = 0u64;

                    loop {
                        let now = Instant::now();
                        if now >= pause_deadline {
                            break;
                        }

                        let remaining = pause_deadline - now;
                        tokio::select! {
                            _ = &mut *stop_rx => {
                                return;
                            }
                            maybe_frame = receiver.recv() => {
                                match maybe_frame {
                                    Some(_) => {
                                        dropped_during_gap += 1;
                                    }
                                    None => {
                                        return;
                                    }
                                }
                            }
                            _ = tokio::time::sleep(remaining) => {
                                break;
                            }
                        }
                    }

                    if dropped_during_gap > 0 {
                        crate::log_warn!(
                            "stream test: dropped {} queued frames during injected gap",
                            dropped_during_gap
                        );
                    }
                    next_send_at = Instant::now();
                }

                let now = Instant::now();
                if now < next_send_at {
                    tokio::time::sleep_until(next_send_at).await;
                } else {
                    let late_ms = now.duration_since(next_send_at).as_secs_f64() * 1000.0;
                    if late_ms > pacer_late_max_ms {
                        pacer_late_max_ms = late_ms;
                    }
                }

                let Ok(opus) = encoder.encode_frame(&frame.samples) else { continue; };
                let packet = build_stream_packet(seq, frame.timestamp_ms, &opus);
                udp.send_to_clients(&packet).await;

                let sent_at = Instant::now();
                if let Some(prev_sent) = last_send_at {
                    let delta_ms = sent_at.duration_since(prev_sent).as_secs_f64() * 1000.0;
                    if delta_ms > send_delta_max_ms {
                        send_delta_max_ms = delta_ms;
                    }
                    if delta_ms > 200.0 {
                        gap_over_200_window += 1;
                        last_interval_was_gap = true;
                        crate::log_warn!("stream warning: send gap {:.2} ms", delta_ms);
                    } else {
                        if last_interval_was_gap && delta_ms < 5.0 {
                            burst_after_gap_window += 1;
                            crate::log_warn!("stream warning: burst after gap delta {:.2} ms", delta_ms);
                        }
                        last_interval_was_gap = false;
                    }
                }

                sent_window += 1;
                last_send_at = Some(sent_at);
                next_send_at += frame_duration;
                if next_send_at < sent_at {
                    next_send_at = sent_at;
                }

                seq = seq.wrapping_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::messages::DEFAULT_STREAM_CONFIG;
    use crate::testing::net_impairment::NetImpairmentController;
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
            UdpServer::bind(SocketAddr::from(([127, 0, 0, 1], 0)), config, 123)
                .await
                .unwrap(),
        );
        let impairment = Arc::new(NetImpairmentController::new());
        let manager = StreamManager::new(audio_source, udp, config, impairment);

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

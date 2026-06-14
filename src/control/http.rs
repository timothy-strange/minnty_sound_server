use crate::audio::capture::PcmFrame;
use crate::audio::controller::AudioSource;
use crate::control::messages::{StreamConfig, build_stream_packet};
use crate::encode::opus::OpusEncoder;
use crate::server_state::ServerState;
use crate::testing::net_impairment::NetImpairmentController;
use crate::transport::udp::UdpServer;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant as StdInstant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

pub const CALIBRATION_STREAM_NAME: &str = "Calibration Stream";

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
    server_state: Arc<ServerState>,
    frame_size: AtomicUsize,
    impairment: Arc<NetImpairmentController>,
    state: Mutex<StreamRuntime>,
}

struct StreamRuntime {
    running: bool,
    sink: Option<String>,
    source: Option<StreamSource>,
    stop_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamSource {
    Capture,
    Calibration,
}

impl StreamManager {
    pub fn new(
        audio: Arc<dyn AudioSource>,
        udp: Arc<UdpServer>,
        config: StreamConfig,
        server_state: Arc<ServerState>,
        impairment: Arc<NetImpairmentController>,
    ) -> Self {
        Self {
            audio,
            udp,
            server_state,
            frame_size: AtomicUsize::new(config.frame_size),
            config,
            impairment,
            state: Mutex::new(StreamRuntime {
                running: false,
                sink: None,
                source: None,
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

        let source = if sink == CALIBRATION_STREAM_NAME {
            StreamSource::Calibration
        } else {
            StreamSource::Capture
        };
        let frame_size = self.frame_size.load(Ordering::Relaxed);
        let effective_config = StreamConfig {
            frame_size,
            ..self.config
        };
        let receiver = match source {
            StreamSource::Capture => {
                self.audio
                    .start_capture(sink.clone(), effective_config)
                    .await?
            }
            StreamSource::Calibration => calibration_stream(effective_config),
        };
        let mut encoder = match OpusEncoder::new(effective_config) {
            Ok(encoder) => encoder,
            Err(err) => {
                if source == StreamSource::Capture {
                    let _ = self.audio.stop_capture().await;
                }
                return Err(err.to_string());
            }
        };
        let udp = Arc::clone(&self.udp);
        let impairment = Arc::clone(&self.impairment);
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let frame_duration = Duration::from_secs_f64(
            effective_config.frame_size as f64 / effective_config.sample_rate as f64,
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
        state.sink = Some(sink.clone());
        state.source = Some(source);
        state.stop_tx = Some(stop_tx);
        state.task = Some(task);
        self.server_state
            .set_current_sink(if source == StreamSource::Capture {
                Some(sink)
            } else {
                None
            })
            .await;

        Ok(())
    }

    pub async fn stop(&self) -> Result<(), String> {
        let (task, source) = {
            let mut state = self.state.lock().await;
            if !state.running {
                return Ok(());
            }

            if let Some(stop_tx) = state.stop_tx.take() {
                let _ = stop_tx.send(());
            }

            state.running = false;
            state.sink = None;
            let source = state.source.take();
            (state.task.take(), source)
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

        if source == Some(StreamSource::Capture) {
            self.audio.stop_capture().await?;
        }
        self.udp.set_streaming(false);
        self.server_state.set_current_sink(None).await;

        Ok(())
    }

    pub fn get_frame_duration_ms(&self) -> u32 {
        let fs = self.frame_size.load(Ordering::Relaxed);
        (fs * 1000 / self.config.sample_rate as usize) as u32
    }

    pub fn set_frame_duration_ms(&self, ms: u32) -> Result<(), String> {
        if ms != 5 && ms != 10 && ms != 20 {
            return Err(format!(
                "Unsupported frame duration: {}ms. Must be 5, 10 or 20.",
                ms
            ));
        }
        let frame_size = ms as usize * self.config.sample_rate as usize / 1000;
        self.frame_size.store(frame_size, Ordering::Relaxed);
        self.udp.set_frame_size(frame_size);
        Ok(())
    }

    pub fn change_server_volume_from_clients(&self) -> bool {
        self.server_state.change_server_volume_from_clients()
    }

    pub fn set_change_server_volume_from_clients(&self, enabled: bool) {
        self.server_state
            .set_change_server_volume_from_clients(enabled);
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

fn calibration_stream(config: StreamConfig) -> tokio::sync::mpsc::Receiver<PcmFrame> {
    let (tx, rx) = tokio::sync::mpsc::channel(config.pcm_queue_depth);
    tokio::spawn(async move {
        let frame_duration =
            Duration::from_secs_f64(config.frame_size as f64 / config.sample_rate as f64);
        let base_wallclock_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let drum_kit = CalibrationDrumKit::new(config.sample_rate);
        let start = StdInstant::now();
        let mut frame_index = 0u64;
        let mut next_frame_at = Instant::now();

        loop {
            tokio::time::sleep_until(next_frame_at).await;
            let timestamp_ms = base_wallclock_ms + start.elapsed().as_millis() as u64;
            let samples = build_calibration_frame(config, frame_index, &drum_kit);
            if tx
                .send(PcmFrame {
                    timestamp_ms,
                    samples,
                })
                .await
                .is_err()
            {
                break;
            }
            frame_index = frame_index.wrapping_add(1);
            next_frame_at += frame_duration;
            let now = Instant::now();
            if next_frame_at < now {
                next_frame_at = now;
            }
        }
    });
    rx
}

struct CalibrationDrumKit {
    sample_rate: u32,
    kick: Vec<f64>,
    snare: Vec<f64>,
    hat: Vec<f64>,
}

impl CalibrationDrumKit {
    fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            kick: generate_kick(sample_rate),
            snare: generate_snare(sample_rate),
            hat: generate_hat(sample_rate),
        }
    }

    fn sample(&self, instrument: &[f64], offset_frames: u64) -> f64 {
        instrument
            .get(offset_frames as usize)
            .copied()
            .unwrap_or(0.0)
    }

    fn sample_at_hit(&self, instrument: &[f64], position_frames: u64, hit_frames: u64) -> f64 {
        if position_frames < hit_frames {
            return 0.0;
        }
        self.sample(instrument, position_frames - hit_frames)
    }
}

fn build_calibration_frame(
    config: StreamConfig,
    frame_index: u64,
    drum_kit: &CalibrationDrumKit,
) -> Vec<i16> {
    const BPM: u64 = 100;
    let quarter_note_frames = drum_kit.sample_rate as u64 * 60 / BPM;
    let eighth_note_frames = quarter_note_frames / 2;
    let bar_frames = quarter_note_frames * 4;

    let channels = config.channels as usize;
    let mut samples = vec![0i16; config.frame_size * channels];
    for frame_offset in 0..config.frame_size {
        let absolute_frame = frame_index * config.frame_size as u64 + frame_offset as u64;
        let bar_pos_frames = absolute_frame % bar_frames;
        let eighth_pos_frames = absolute_frame % eighth_note_frames;

        let mut sample = 0.0;
        sample += drum_kit.sample_at_hit(&drum_kit.kick, bar_pos_frames, 0);
        sample += drum_kit.sample_at_hit(&drum_kit.kick, bar_pos_frames, quarter_note_frames * 2);
        sample += drum_kit.sample_at_hit(&drum_kit.snare, bar_pos_frames, quarter_note_frames);
        sample += drum_kit.sample_at_hit(&drum_kit.snare, bar_pos_frames, quarter_note_frames * 3);
        sample += drum_kit.sample(&drum_kit.hat, eighth_pos_frames);

        let pcm = (sample.clamp(-1.0, 1.0) * i16::MAX as f64 * 0.75) as i16;
        for channel in 0..channels {
            samples[frame_offset * channels + channel] = pcm;
        }
    }
    samples
}

fn generate_kick(sample_rate: u32) -> Vec<f64> {
    let len = frames_for_ms(sample_rate, 170);
    let mut samples = Vec::with_capacity(len);
    let mut phase = 0.0;
    for n in 0..len {
        let t = n as f64 / sample_rate as f64;
        let progress = n as f64 / len as f64;
        let pitch = 45.0 + 95.0 * (1.0 - progress).powi(3);
        phase += std::f64::consts::TAU * pitch / sample_rate as f64;
        let body = phase.sin() * (-t * 30.0).exp();
        let click = if n < frames_for_ms(sample_rate, 5) {
            noise_sample(n as u64) * (1.0 - n as f64 / frames_for_ms(sample_rate, 5) as f64)
        } else {
            0.0
        };
        samples.push((body * 1.15 + click * 0.28).tanh() * 0.95);
    }
    samples
}

fn generate_snare(sample_rate: u32) -> Vec<f64> {
    eprintln!("SNARE v8");
    let len = frames_for_ms(sample_rate, 120);
    let fade_start = frames_for_ms(sample_rate, 100);
    let mut samples = Vec::with_capacity(len);

    // Body: two fixed-frequency bridged-T resonators (808 circuit: ~200 Hz + ~390 Hz)
    let mut phase1 = 0.0f64;
    let mut phase2 = 0.0f64;

    // Snare wire: stateful PRNG for true white noise
    let mut rng: u64 = 0xDEADBEEF13371337;

    // 2-pole Sallen-Key HPF at 2500 Hz
    let hp_a = 1.0 / (1.0 + std::f64::consts::TAU * 2500.0 / sample_rate as f64);
    let mut hp1_px = 0.0f64;
    let mut hp1_py = 0.0f64;
    let mut hp2_px = 0.0f64;
    let mut hp2_py = 0.0f64;

    for n in 0..len {
        let t = n as f64 / sample_rate as f64;

        // Body: fast decay for a tight thump
        phase1 += std::f64::consts::TAU * 200.0 / sample_rate as f64;
        phase2 += std::f64::consts::TAU * 390.0 / sample_rate as f64;
        let body_env = (-t * 55.0).exp();
        let body = (phase1.sin() * 0.6 + phase2.sin() * 0.4) * body_env;

        // Snare wire: true white noise → VCA → 2-pole HPF at 2500 Hz
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let raw = (rng >> 32) as f64 / 2147483648.0 - 1.0;
        let wire_env = (-t * 45.0).exp();
        let shaped = raw * wire_env;
        let hp1 = hp_a * (hp1_py + shaped - hp1_px);
        hp1_px = shaped;
        hp1_py = hp1;
        let hp2 = hp_a * (hp2_py + hp1 - hp2_px);
        hp2_px = hp1;
        hp2_py = hp2;

        // Attack crack: extra noise burst in the first few ms
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let crack = (rng >> 32) as f64 / 2147483648.0 - 1.0;
        let crack_env = (-t * 200.0).exp();

        // Hard gate: linear fade over the last 20ms to cut cleanly without a click
        let gate = if n < fade_start {
            1.0
        } else {
            1.0 - (n - fade_start) as f64 / (len - fade_start) as f64
        };

        // Drive harder into tanh for more punch and grit
        samples.push((body * 0.50 + hp2 * 1.40 + crack * crack_env * 0.70).tanh() * 0.85 * gate);
    }
    samples
}

fn generate_hat(sample_rate: u32) -> Vec<f64> {
    let len = frames_for_ms(sample_rate, 60);
    let mut samples = Vec::with_capacity(len);

    // True white noise source
    let mut rng: u64 = 0xCAFEBABE87654321;

    // 2-pole HPF at 6000 Hz — hi-hat lives almost entirely above this
    let hp_a = 1.0 / (1.0 + std::f64::consts::TAU * 6000.0 / sample_rate as f64);
    let mut hp1_px = 0.0f64;
    let mut hp1_py = 0.0f64;
    let mut hp2_px = 0.0f64;
    let mut hp2_py = 0.0f64;

    for n in 0..len {
        let t = n as f64 / sample_rate as f64;

        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let raw = (rng >> 32) as f64 / 2147483648.0 - 1.0;

        let hp1 = hp_a * (hp1_py + raw - hp1_px);
        hp1_px = raw;
        hp1_py = hp1;
        let hp2 = hp_a * (hp2_py + hp1 - hp2_px);
        hp2_px = hp1;
        hp2_py = hp2;

        let env = (-t * 80.0).exp();
        samples.push(hp2 * env * 0.55);
    }
    samples
}

fn frames_for_ms(sample_rate: u32, ms: u64) -> usize {
    ((sample_rate as u64 * ms) / 1_000) as usize
}

fn noise_sample(index: u64) -> f64 {
    let mut x = index
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    x ^= x >> 33;
    let value = ((x >> 32) & 0xffff) as f64 / 32768.0 - 1.0;
    value
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
        let manager = StreamManager::new(
            audio_source,
            udp,
            config,
            Arc::new(ServerState::new()),
            impairment,
        );

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

    #[tokio::test]
    async fn calibration_stream_does_not_start_capture() {
        let config = DEFAULT_STREAM_CONFIG;
        let audio = Arc::new(MockAudio::new());
        let audio_source: Arc<dyn AudioSource> = audio.clone();
        let udp = Arc::new(
            UdpServer::bind(SocketAddr::from(([127, 0, 0, 1], 0)), config, 123)
                .await
                .unwrap(),
        );
        let impairment = Arc::new(NetImpairmentController::new());
        let manager = StreamManager::new(
            audio_source,
            udp,
            config,
            Arc::new(ServerState::new()),
            impairment,
        );

        manager
            .start(CALIBRATION_STREAM_NAME.to_string())
            .await
            .unwrap();
        let status = manager.status().await;
        assert!(status.running);
        assert_eq!(status.sink.as_deref(), Some(CALIBRATION_STREAM_NAME));

        manager.stop().await.unwrap();

        assert_eq!(audio.start_calls.load(Ordering::SeqCst), 0);
        assert_eq!(audio.stop_calls.load(Ordering::SeqCst), 0);
    }
}

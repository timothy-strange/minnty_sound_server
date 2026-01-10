use crate::control::messages::{MAGIC, MSG_HELLO, MSG_KEEPALIVE, StreamConfig, VERSION};
use hound::WavWriter;
use opus::{Channels, Decoder};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

const HEADER_SIZE: usize = 1 + 4 + 8 + 2;

#[derive(Clone, Copy, Debug)]
pub struct MonitorStatus {
    pub active: bool,
    pub peak: f32,
}

pub struct MonitorManager {
    config: StreamConfig,
    peak: Arc<AtomicU32>,
    state: Mutex<MonitorState>,
}

struct MonitorState {
    active: bool,
    stop_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl MonitorManager {
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

    pub async fn start(&self, sink: String) -> Result<(), String> {
        let mut state = self.state.lock().await;
        if state.active {
            return Err("Monitor already running".to_string());
        }

        let recording_path = build_recording_path(&sink).map_err(|err| err.to_string())?;
        let writer = WavWriter::create(&recording_path, wav_spec(self.config))
            .map_err(|err| err.to_string())?;

        let (stop_tx, mut stop_rx) = oneshot::channel();
        let peak = Arc::clone(&self.peak);
        let config = self.config;

        let task = tokio::spawn(async move {
            run_monitor(config, peak, writer, &mut stop_rx).await;
        });

        state.active = true;
        state.stop_tx = Some(stop_tx);
        state.task = Some(task);

        Ok(())
    }

    pub async fn stop(&self) {
        let mut state = self.state.lock().await;
        if !state.active {
            return;
        }

        if let Some(stop_tx) = state.stop_tx.take() {
            let _ = stop_tx.send(());
        }

        if let Some(task) = state.task.take() {
            let _ = task.await;
        }

        self.peak.store(0f32.to_bits(), Ordering::Relaxed);
        state.active = false;
    }

    pub async fn status(&self) -> MonitorStatus {
        let state = self.state.lock().await;
        MonitorStatus {
            active: state.active,
            peak: f32::from_bits(self.peak.load(Ordering::Relaxed)),
        }
    }
}

async fn run_monitor(
    config: StreamConfig,
    peak: Arc<AtomicU32>,
    mut writer: WavWriter<std::io::BufWriter<std::fs::File>>,
    stop_rx: &mut oneshot::Receiver<()>,
) {
    let socket = match UdpSocket::bind("127.0.0.1:0").await {
        Ok(socket) => socket,
        Err(_) => return,
    };
    let server_addr = SocketAddr::from(([127, 0, 0, 1], config.udp_port));
    let channels = match config.channels {
        1 => Channels::Mono,
        _ => Channels::Stereo,
    };
    let mut decoder = match Decoder::new(config.sample_rate, channels) {
        Ok(decoder) => decoder,
        Err(_) => return,
    };

    let hello = build_control_packet(MSG_HELLO);
    let keepalive = build_control_packet(MSG_KEEPALIVE);
    let _ = socket.send_to(&hello, server_addr).await;

    let mut interval = tokio::time::interval(Duration::from_secs(2));
    let mut buffer = vec![0u8; 4096];
    let mut pcm = vec![0i16; config.frame_size * config.channels as usize];

    loop {
        tokio::select! {
            _ = &mut *stop_rx => {
                break;
            }
            _ = interval.tick() => {
                let _ = socket.send_to(&keepalive, server_addr).await;
            }
            result = socket.recv_from(&mut buffer) => {
                let Ok((len, _addr)) = result else { continue; };
                if len < HEADER_SIZE {
                    continue;
                }

                if buffer[0] != VERSION {
                    continue;
                }

                let payload_len = u16::from_be_bytes([buffer[13], buffer[14]]) as usize;
                if HEADER_SIZE + payload_len > len {
                    continue;
                }

                let payload = &buffer[15..15 + payload_len];
                let Ok(samples) = decoder.decode(payload, &mut pcm, false) else { continue; };
                let sample_count = samples * config.channels as usize;
                let mut max = 0.0f32;
                for &sample in pcm.iter().take(sample_count) {
                    let value = (sample as f32 / i16::MAX as f32).abs();
                    if value > max {
                        max = value;
                    }
                    let _ = writer.write_sample(sample);
                }
                peak.store(max.to_bits(), Ordering::Relaxed);
            }
        }
    }

    let _ = writer.finalize();
}

fn build_control_packet(msg_type: u8) -> Vec<u8> {
    let mut buf = Vec::with_capacity(6);
    buf.extend_from_slice(&MAGIC);
    buf.push(VERSION);
    buf.push(msg_type);
    buf
}

fn wav_spec(config: StreamConfig) -> hound::WavSpec {
    hound::WavSpec {
        channels: config.channels as u16,
        sample_rate: config.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }
}

fn build_recording_path(sink: &str) -> Result<std::path::PathBuf, std::io::Error> {
    let mut dir = std::path::PathBuf::from("recordings");
    std::fs::create_dir_all(&dir)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let sanitized = sink
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>();
    dir.push(format!("{}_{}.wav", sanitized, timestamp));
    Ok(dir)
}

use crate::audio::capture::{CaptureSession, PcmFrame};
use crate::control::messages::StreamConfig;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use wasapi::{Device, DeviceCollection, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat, initialize_mta};

struct SinkMeter {
    name: String,
    peak: Arc<AtomicU32>,
}

pub struct PulseManager {
    meters: Vec<SinkMeter>,
    meters_running: bool,
}

impl PulseManager {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let _ = initialize_mta().ok();
        let meters = enumerate_render_devices()?;
        Ok(Self {
            meters,
            meters_running: false,
        })
    }

    pub fn start_meters(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.meters.is_empty() {
            self.meters = enumerate_render_devices()?;
        }
        self.meters_running = true;
        Ok(())
    }

    pub fn export_meters(&self) -> Vec<(String, Arc<AtomicU32>)> {
        self.meters
            .iter()
            .map(|meter| (meter.name.clone(), Arc::clone(&meter.peak)))
            .collect()
    }

    pub fn stop_meters(&mut self) {
        self.meters_running = false;
    }

    pub fn start_meters_if_stopped(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.meters_running {
            return Ok(());
        }
        self.start_meters()
    }

    pub fn start_capture(
        &mut self,
        sink_name: &str,
        config: StreamConfig,
    ) -> Result<(CaptureSession, mpsc::Receiver<PcmFrame>), Box<dyn std::error::Error>> {
        let (tx, rx) = mpsc::channel(config.pcm_queue_depth);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_worker = Arc::clone(&stop_flag);
        let sink_name = sink_name.to_owned();

        let worker = thread::spawn(move || {
            if let Err(err) = capture_worker(stop_flag_worker, tx, &sink_name, config) {
                crate::log_warn!("audio warning: windows capture worker failed: {err}");
            }
        });

        Ok((CaptureSession::new(stop_flag, worker), rx))
    }

    pub fn stop_capture(&mut self, mut capture: CaptureSession) {
        capture.shutdown();
    }
}

fn enumerate_render_devices() -> Result<Vec<SinkMeter>, Box<dyn std::error::Error>> {
    let _ = initialize_mta().ok();
    let enumerator = DeviceEnumerator::new()?;
    let collection = enumerator.get_device_collection(&Direction::Render)?;

    let mut meters = Vec::new();
    for device in &collection {
        let device = device?;
        let name = device.get_friendlyname()?;
        meters.push(SinkMeter {
            name,
            peak: Arc::new(AtomicU32::new(0f32.to_bits())),
        });
    }

    if meters.is_empty() {
        let default = enumerator.get_default_device(&Direction::Render)?;
        meters.push(SinkMeter {
            name: default.get_friendlyname()?,
            peak: Arc::new(AtomicU32::new(0f32.to_bits())),
        });
    }

    Ok(meters)
}

fn capture_worker(
    stop_flag: Arc<AtomicBool>,
    tx: mpsc::Sender<PcmFrame>,
    sink_name: &str,
    config: StreamConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = initialize_mta().ok();
    let enumerator = DeviceEnumerator::new()?;
    let device = select_render_device(&enumerator, sink_name)?;
    let mut audio_client = device.get_iaudioclient()?;
    let desired_format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        config.sample_rate as usize,
        config.channels as usize,
        None,
    );

    let (_, min_time) = audio_client.get_device_period()?;
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: min_time,
    };
    audio_client.initialize_client(&desired_format, &Direction::Render, &mode)?;
    let h_event = audio_client.set_get_eventhandle()?;
    let capture_client = audio_client.get_audiocaptureclient()?;
    let buffer_frame_count = audio_client.get_buffer_size()?;
    let blockalign = desired_format.get_blockalign() as usize;
    let frame_bytes = blockalign * config.frame_size;
    let mut sample_queue: VecDeque<u8> = VecDeque::with_capacity(frame_bytes * 4 + buffer_frame_count as usize * blockalign);
    let base_wallclock_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let capture_start = Instant::now();

    audio_client.start_stream()?;

    while !stop_flag.load(Ordering::Acquire) {
        capture_client.read_from_device_to_deque(&mut sample_queue)?;

        while sample_queue.len() >= frame_bytes {
            let mut chunk = vec![0u8; frame_bytes];
            for byte in &mut chunk {
                *byte = sample_queue.pop_front().unwrap();
            }
            let samples = float_bytes_to_i16_samples(&chunk);
            let frame = PcmFrame {
                timestamp_ms: base_wallclock_ms + capture_start.elapsed().as_millis() as u64,
                samples,
            };

            match tx.try_send(frame) {
                Ok(_) => {}
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Closed(_)) => {
                    let _ = audio_client.stop_stream();
                    return Ok(());
                }
            }
        }

        if h_event.wait_for_event(100000).is_err() {
            break;
        }
    }

    let _ = audio_client.stop_stream();
    Ok(())
}

fn select_render_device(
    enumerator: &DeviceEnumerator,
    sink_name: &str,
) -> Result<Device, Box<dyn std::error::Error>> {
    let collection: DeviceCollection = enumerator.get_device_collection(&Direction::Render)?;
    if !sink_name.is_empty() {
        if let Ok(device) = collection.get_device_with_name(sink_name) {
            return Ok(device);
        }
    }
    Ok(enumerator.get_default_device(&Direction::Render)?)
}

fn float_bytes_to_i16_samples(bytes: &[u8]) -> Vec<i16> {
    let mut samples = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        let sample = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let clamped = sample.clamp(-1.0, 1.0);
        samples.push((clamped * i16::MAX as f32) as i16);
    }
    samples
}

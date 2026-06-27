use crate::audio::capture::{CaptureSession, PcmFrame};
use crate::control::messages::StreamConfig;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use wasapi::{
    AudioClient, Device, DeviceCollection, DeviceEnumerator, Direction, SampleType, ShareMode,
    StreamMode, WaveFormat, initialize_mta,
};

struct SinkMeter {
    name: String,
    peak: Arc<AtomicU32>,
    stream: Option<MeterHandle>,
}

struct MeterHandle {
    stop_flag: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl MeterHandle {
    fn new(stop_flag: Arc<AtomicBool>, worker: JoinHandle<()>) -> Self {
        Self {
            stop_flag,
            worker: Some(worker),
        }
    }

    fn shutdown(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
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
        if self.meters_running {
            return Ok(());
        }

        if self.meters.is_empty() {
            self.meters = enumerate_render_devices()?;
        }

        for meter in &mut self.meters {
            let name = meter.name.clone();
            let peak = Arc::clone(&meter.peak);
            meter.stream = Some(start_sink_monitor(&name, peak));
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
        if !self.meters_running {
            return;
        }

        for meter in &mut self.meters {
            if let Some(mut handle) = meter.stream.take() {
                handle.shutdown();
            }
        }

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
            stream: None,
        });
    }

    if meters.is_empty() {
        let default = enumerator.get_default_device(&Direction::Render)?;
        meters.push(SinkMeter {
            name: default.get_friendlyname()?,
            peak: Arc::new(AtomicU32::new(0f32.to_bits())),
            stream: None,
        });
    }

    Ok(meters)
}

fn start_sink_monitor(sink_name: &str, peak: Arc<AtomicU32>) -> MeterHandle {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_worker = Arc::clone(&stop_flag);
    let sink_name = sink_name.to_owned();

    let worker = thread::spawn(move || {
        if let Err(err) = meter_worker(stop_flag_worker, &sink_name, peak) {
            crate::log_warn!(
                "audio warning: windows meter worker failed sink={}: {err}",
                sink_name
            );
        }
    });

    MeterHandle::new(stop_flag, worker)
}

fn meter_worker(
    stop_flag: Arc<AtomicBool>,
    sink_name: &str,
    peak: Arc<AtomicU32>,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = initialize_mta().ok();
    let enumerator = DeviceEnumerator::new()?;
    let device = select_render_device(&enumerator, sink_name)?;
    let mut audio_client = device.get_iaudioclient()?;
    let desired_format = stream_format(&audio_client, 48_000, 2)?;

    let (_, min_time) = audio_client.get_device_period()?;
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: min_time,
    };
    audio_client.initialize_client(&desired_format, &Direction::Capture, &mode)?;
    let h_event = audio_client.set_get_eventhandle()?;
    let capture_client = audio_client.get_audiocaptureclient()?;
    let buffer_frame_count = audio_client.get_buffer_size()?;
    let blockalign = desired_format.get_blockalign() as usize;
    let mut sample_queue: VecDeque<u8> =
        VecDeque::with_capacity(buffer_frame_count as usize * blockalign * 2);
    audio_client.start_stream()?;

    while !stop_flag.load(Ordering::Acquire) {
        capture_client.read_from_device_to_deque(&mut sample_queue)?;
        if !sample_queue.is_empty() {
            let bytes = sample_queue.make_contiguous();
            peak.store(float_bytes_peak(bytes).to_bits(), Ordering::Relaxed);
            sample_queue.clear();
        }

        let _ = h_event.wait_for_event(100);
    }

    let _ = audio_client.stop_stream();
    Ok(())
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
    let desired_format = stream_format(
        &audio_client,
        config.sample_rate as usize,
        config.channels as usize,
    )?;

    let (_, min_time) = audio_client.get_device_period()?;
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: min_time,
    };
    audio_client.initialize_client(&desired_format, &Direction::Capture, &mode)?;
    let h_event = audio_client.set_get_eventhandle()?;
    let capture_client = audio_client.get_audiocaptureclient()?;
    let buffer_frame_count = audio_client.get_buffer_size()?;
    let blockalign = desired_format.get_blockalign() as usize;
    let frame_bytes = blockalign * config.frame_size;
    let mut sample_queue: VecDeque<u8> =
        VecDeque::with_capacity(frame_bytes * 4 + buffer_frame_count as usize * blockalign);
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
                timestamp_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
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

        let _ = h_event.wait_for_event(100);
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

fn stream_format(
    audio_client: &AudioClient,
    sample_rate: usize,
    channels: usize,
) -> Result<WaveFormat, Box<dyn std::error::Error>> {
    let candidates = [
        WaveFormat::new(32, 32, &SampleType::Float, sample_rate, channels, None),
        WaveFormat::new(32, 32, &SampleType::Float, sample_rate, channels, Some(0)),
        WaveFormat::new(32, 32, &SampleType::Float, sample_rate, channels, None)
            .to_waveformatex()?,
    ];

    for candidate in candidates {
        if audio_client
            .is_supported(&candidate, &ShareMode::Shared)
            .is_ok()
        {
            return Ok(candidate);
        }
    }

    Err(format!(
        "No supported WASAPI shared format for {} Hz, {} channel float audio",
        sample_rate, channels
    )
    .into())
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

fn float_bytes_peak(bytes: &[u8]) -> f32 {
    let mut max = 0.0f32;
    for chunk in bytes.chunks_exact(4) {
        let sample = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        max = max.max(sample.abs().min(1.0));
    }
    max
}

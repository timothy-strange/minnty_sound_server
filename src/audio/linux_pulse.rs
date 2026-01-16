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

struct SinkMeter {
    name: String,
    peak: Arc<AtomicU32>,
    stream: Option<PulseStreamHandle>,
}

pub struct PulseManager {
    mainloop: Mainloop,
    context: Context,
    meters: Vec<SinkMeter>,
}

impl PulseManager {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let mut mainloop = Mainloop::new().ok_or("Failed to create PulseAudio mainloop")?;
        let mut context =
            Context::new(&mainloop, "MinntyServer").ok_or("Failed to create PulseAudio context")?;

        mainloop.start()?;

        mainloop.lock();
        context.connect(None, FlagSet::NOFLAGS, None)?;
        mainloop.unlock();

        let mut manager = Self {
            mainloop,
            context,
            meters: Vec::new(),
        };

        manager.wait_for_ready()?;
        Ok(manager)
    }

    fn wait_for_ready(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let start = Instant::now();
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

    pub fn start_meters(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        use pulse::callbacks::ListResult;

        enum Msg {
            Item(String),
            End,
            Error,
        }

        let (tx, rx) = std::sync::mpsc::channel::<Msg>();

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

        while let Ok(msg) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
            match msg {
                Msg::Item(name) => {
                    let peak = Arc::new(AtomicU32::new(0f32.to_bits()));
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

    fn start_sink_monitor(
        &mut self,
        sink_name: &str,
        peak: Arc<AtomicU32>,
    ) -> Result<PulseStreamHandle, Box<dyn std::error::Error>> {
        let spec = pulse::sample::Spec {
            format: Format::S16le,
            rate: 44100,
            channels: 2,
        };

        let buffer_attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: 1024,
        };

        self.mainloop.lock();

        let stream =
            Stream::new(&mut self.context, "meter", &spec, None).ok_or("Stream creation failed")?;
        let mut handle = PulseStreamHandle::new(stream, &self.mainloop);
        let stream_ptr = handle.as_ptr();

        unsafe {
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

            (*stream_ptr).connect_record(
                Some(&format!("{}.monitor", sink_name)),
                Some(&buffer_attr),
                StreamFlagSet::ADJUST_LATENCY,
            )?;
        }

        self.mainloop.unlock();
        Ok(handle)
    }

    pub fn start_capture(
        &mut self,
        sink_name: &str,
        config: StreamConfig,
    ) -> Result<(CaptureSession, mpsc::Receiver<PcmFrame>), Box<dyn std::error::Error>> {
        let spec = pulse::sample::Spec {
            format: Format::S16le,
            rate: config.sample_rate,
            channels: config.channels,
        };

        let frame_samples = config.frame_size * config.channels as usize;
        let frag_bytes = (frame_samples * 2) as u32;

        let buffer_attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: frag_bytes,
        };

        let (tx, rx) = mpsc::channel(config.pcm_queue_depth);
        let tx = tx.clone();

        self.mainloop.lock();

        let stream = Stream::new(&mut self.context, "capture", &spec, None)
            .ok_or("Capture stream creation failed")?;
        let mut handle = PulseStreamHandle::new(stream, &self.mainloop);
        let stream_ptr = handle.as_ptr();
        let mut ring = RingBuffer::new(frame_samples * 16);

        unsafe {
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
                            }
                            Err(TrySendError::Closed(_)) => return,
                        }
                    }
                }
            })));

            (*stream_ptr).connect_record(
                Some(&format!("{}.monitor", sink_name)),
                Some(&buffer_attr),
                StreamFlagSet::ADJUST_LATENCY,
            )?;
        }

        self.mainloop.unlock();
        Ok((CaptureSession::new(handle), rx))
    }

    pub fn stop_capture(&mut self, mut capture: CaptureSession) {
        capture.shutdown();
    }

    fn cleanup_meters(&mut self) {
        for meter in &mut self.meters {
            if let Some(handle) = meter.stream.as_mut() {
                handle.shutdown();
            }
        }
    }

    pub fn export_meters(&self) -> Vec<(String, Arc<AtomicU32>)> {
        self.meters
            .iter()
            .map(|m| (m.name.clone(), Arc::clone(&m.peak)))
            .collect()
    }
}

impl Drop for SinkMeter {
    fn drop(&mut self) {
        if let Some(handle) = self.stream.as_mut() {
            handle.shutdown();
        }
    }
}

impl Drop for PulseManager {
    fn drop(&mut self) {
        self.cleanup_meters();
        self.mainloop.lock();
        self.context.disconnect();
        self.mainloop.unlock();
        self.mainloop.stop();
    }
}
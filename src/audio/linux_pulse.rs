use crate::audio::capture::{CaptureSession, PcmFrame, PulseStreamHandle};
use crate::control::messages::StreamConfig;
use libpulse_binding as pulse;
use pulse::context::{Context, FlagSet, State};
use pulse::def::BufferAttr;
use pulse::mainloop::threaded::Mainloop;
use pulse::sample::Format;
use pulse::stream::{FlagSet as StreamFlagSet, Stream};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
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
    meters_running: bool,
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
            meters_running: false,
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
        if self.meters_running {
            return Ok(());
        }

        if !self.meters.is_empty() {
            for idx in 0..self.meters.len() {
                let name = self.meters[idx].name.clone();
                let peak = Arc::clone(&self.meters[idx].peak);
                let stream = self.start_sink_monitor(&name, peak)?;
                self.meters[idx].stream = Some(stream);
            }
            self.meters_running = true;
            return Ok(());
        }

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

        self.meters_running = true;
        Ok(())
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
        let ring_capacity = frame_samples
            .saturating_mul(config.pcm_queue_depth)
            .saturating_mul(2)
            .max(frame_samples);
        let (mut producer, mut consumer) = rtrb::RingBuffer::<i16>::new(ring_capacity);

        let ring_drop_newest = Arc::new(AtomicU64::new(0));
        let ring_drop_newest_cb = Arc::clone(&ring_drop_newest);
        let callback_gap_max_ms = Arc::new(AtomicU64::new(0));
        let callback_gap_max_ms_cb = Arc::clone(&callback_gap_max_ms);
        let callback_gap_over_200 = Arc::new(AtomicU64::new(0));
        let callback_gap_over_200_cb = Arc::clone(&callback_gap_over_200);
        let pending_sample_count = Arc::new(AtomicUsize::new(0));
        let pending_sample_count_cb = Arc::clone(&pending_sample_count);
        let ring_backlog_warn_threshold = frame_samples.saturating_mul(config.pcm_queue_depth);

        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_worker = Arc::clone(&stop_flag);

        let worker = std::thread::spawn(move || {
            const MAX_WORKER_FRAMES_PER_CYCLE: usize = 3;
            let max_samples_per_cycle = frame_samples * MAX_WORKER_FRAMES_PER_CYCLE;
            let mut pending_samples: Vec<i16> = Vec::with_capacity(max_samples_per_cycle * 2);
            let mut emitted_window = 0u64;
            let mut dropped_full_window = 0u64;
            let mut backlog_max_window = 0usize;
            let mut last_summary = Instant::now();

            loop {
                let mut pulled = 0usize;
                while pulled < max_samples_per_cycle {
                    match consumer.pop() {
                        Ok(sample) => {
                            let _ = pending_sample_count.fetch_update(
                                Ordering::Relaxed,
                                Ordering::Relaxed,
                                |n| Some(n.saturating_sub(1)),
                            );
                            pending_samples.push(sample);
                            pulled += 1;
                        }
                        Err(_) => break,
                    }
                }

                let raw_backlog = pending_sample_count.load(Ordering::Relaxed);
                let backlog = if raw_backlog > ring_capacity {
                    pending_sample_count.store(0, Ordering::Relaxed);
                    0
                } else {
                    raw_backlog
                };
                if backlog > backlog_max_window {
                    backlog_max_window = backlog;
                }

                let mut emitted = 0usize;
                while pending_samples.len() >= frame_samples
                    && emitted < MAX_WORKER_FRAMES_PER_CYCLE
                {
                    let frame_samples_vec: Vec<i16> =
                        pending_samples.drain(..frame_samples).collect();
                    let timestamp_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let frame = PcmFrame {
                        timestamp_ms,
                        samples: frame_samples_vec,
                    };

                    match tx.try_send(frame) {
                        Ok(_) => {
                            emitted += 1;
                            emitted_window += 1;
                        }
                        Err(TrySendError::Full(_)) => {
                            dropped_full_window += 1;
                        }
                        Err(TrySendError::Closed(_)) => return,
                    }
                }

                let now = Instant::now();
                if now.duration_since(last_summary) >= Duration::from_secs(2) {
                    let ring_drop_newest_window = ring_drop_newest.swap(0, Ordering::Relaxed);
                    let callback_gap_max_window = callback_gap_max_ms.swap(0, Ordering::Relaxed);
                    let callback_gap_over_200_window =
                        callback_gap_over_200.swap(0, Ordering::Relaxed);

                    crate::log_info!(
                        "capture diag: emitted={} droppedFull={} ringDroppedNewest={} callbackGapMaxMs={} callbackGapOver200={} framesLeftInRingMax={}",
                        emitted_window,
                        dropped_full_window,
                        ring_drop_newest_window,
                        callback_gap_max_window,
                        callback_gap_over_200_window,
                        backlog_max_window
                    );

                    if callback_gap_max_window > 200 {
                        crate::log_warn!(
                            "capture warning: callback gap {} ms",
                            callback_gap_max_window
                        );
                    }
                    if backlog_max_window > ring_backlog_warn_threshold {
                        crate::log_warn!(
                            "capture warning: ring backlog high {} samples (threshold {})",
                            backlog_max_window,
                            ring_backlog_warn_threshold
                        );
                    }

                    emitted_window = 0;
                    dropped_full_window = 0;
                    backlog_max_window = 0;
                    last_summary = now;
                }

                if stop_flag_worker.load(std::sync::atomic::Ordering::Acquire)
                    && pending_samples.len() < frame_samples
                {
                    break;
                }

                if pulled == 0 && emitted == 0 {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        });

        self.mainloop.lock();

        let stream = Stream::new(&mut self.context, "capture", &spec, None)
            .ok_or("Capture stream creation failed")?;
        let mut handle = PulseStreamHandle::new(stream, &self.mainloop);
        let stream_ptr = handle.as_ptr();
        let mut last_callback_at: Option<Instant> = None;

        unsafe {
            (*stream_ptr).set_read_callback(Some(Box::new(move |_n| {
                let s = &mut *stream_ptr;
                let callback_now = Instant::now();
                if let Some(last) = last_callback_at {
                    let gap_ms = callback_now.duration_since(last).as_millis() as u64;
                    callback_gap_max_ms_cb.fetch_max(gap_ms, Ordering::Relaxed);
                    if gap_ms > 200 {
                        callback_gap_over_200_cb.fetch_add(1, Ordering::Relaxed);
                    }
                }
                last_callback_at = Some(callback_now);

                while let Ok(pulse::stream::PeekResult::Data(data)) = s.peek() {
                    let samples =
                        std::slice::from_raw_parts(data.as_ptr() as *const i16, data.len() / 2);
                    for &sample in samples {
                        if producer.push(sample).is_err() {
                            // Drop-newest policy when bounded SPSC buffer is full.
                            ring_drop_newest_cb.fetch_add(1, Ordering::Relaxed);
                        } else {
                            pending_sample_count_cb.fetch_add(1, Ordering::Relaxed);
                        }
                    }
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
        Ok((CaptureSession::new(handle, stop_flag, worker), rx))
    }

    pub fn stop_capture(&mut self, mut capture: CaptureSession) {
        capture.shutdown();
    }

    fn cleanup_meters(&mut self) {
        for meter in &mut self.meters {
            if let Some(mut handle) = meter.stream.take() {
                handle.shutdown();
            }
        }
        self.meters_running = false;
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

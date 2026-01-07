use libpulse_binding as pulse;
use pulse::context::{Context, FlagSet, State};
use pulse::mainloop::threaded::Mainloop;
use pulse::sample::Format;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use pulse::stream::{Stream, FlagSet as StreamFlagSet};
use pulse::sample::Spec;

#[derive(Debug, Clone)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u8,
    pub format: Format,
}

pub struct PulseManager {
    mainloop: Mainloop,
    context: Context,
}

impl PulseManager {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let mut mainloop =
            Mainloop::new().ok_or("Failed to create PulseAudio mainloop")?;
        let mut context =
            Context::new(&mainloop, "Minnty Sound Server")
                .ok_or("Failed to create PulseAudio context")?;

        mainloop.start()?;

        mainloop.lock();
        context.connect(None, FlagSet::NOFLAGS, None)?;
        mainloop.unlock();

        let mut manager = Self { mainloop, context };
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
                    return Err("PulseAudio connection failed".into())
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

    pub fn get_default_device_info(
        &mut self,
    ) -> Result<AudioDeviceInfo, Box<dyn std::error::Error>> {
        let (tx, rx) = mpsc::channel();

        self.mainloop.lock();
        self.context
            .introspect()
            .get_server_info(move |info| {
                let _ = tx.send(
                    info.default_sink_name
                        .as_ref()
                        .map(|s| s.to_string()),
                );
            });
        self.mainloop.unlock();

        let sink_name = rx
            .recv_timeout(Duration::from_secs(2))?
            .ok_or("No default sink")?;

        let (tx, rx) = mpsc::channel();

        self.mainloop.lock();
        self.context
            .introspect()
            .get_sink_info_by_name(&sink_name, move |res| {
                if let pulse::callbacks::ListResult::Item(i) = res {
                    let info = AudioDeviceInfo {
                        name: i
                            .name
                            .as_ref()
                            .map(|s| s.to_string())
                            .unwrap_or_default(),
                        sample_rate: i.sample_spec.rate,
                        channels: i.sample_spec.channels,
                        format: i.sample_spec.format,
                    };
                    let _ = tx.send(Some(info));
                }
            });
        self.mainloop.unlock();

        rx.recv_timeout(Duration::from_secs(2))?
            .ok_or("Failed to get sink info".into())
    }
    
    pub fn start_background_capture(
        &mut self,
        is_running: std::sync::Arc<std::sync::atomic::AtomicBool>, // Add this parameter
    ) -> Result<std::sync::Arc<std::sync::atomic::AtomicU32>, Box<dyn std::error::Error>> {
        use pulse::sample::Spec;
        use pulse::stream::{FlagSet, Stream};
        use pulse::def::BufferAttr;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let peak_level = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let peak_ptr = Arc::clone(&peak_level);

        let spec = Spec {
            format: pulse::sample::Format::S16le,
            rate: 44100,
            channels: 2,
        };

        let buffer_attr = BufferAttr {
            maxlength: u32::MAX, tlength: u32::MAX, prebuf: u32::MAX, minreq: u32::MAX, fragsize: 1024,
        };

        self.mainloop.lock();
        let stream = Stream::new(&mut self.context, "CaptureStream", &spec, None)
            .ok_or("Failed to create stream")?;
        let stream_ptr = Box::into_raw(Box::new(stream));

        unsafe {
            (*stream_ptr).set_read_callback(Some(Box::new(move |_bytes| {
                let s = &mut *stream_ptr;
                
                while let Ok(pulse::stream::PeekResult::Data(data)) = s.peek() {
                    // ONLY process if the server is toggled ON
                    if is_running.load(Ordering::Relaxed) {
                        let samples = std::slice::from_raw_parts(data.as_ptr() as *const i16, data.len() / 2);
                        let mut max_vol = 0.0f32;
                        for &sample in samples {
                            let val = (sample as f32 / i16::MAX as f32).abs();
                            if val > max_vol { max_vol = val; }
                        }
                        peak_ptr.store(max_vol.to_bits(), Ordering::Relaxed);
                    } else {
                        // Reset meter when stopped
                        peak_ptr.store(0.0f32.to_bits(), Ordering::Relaxed);
                    }
                    let _ = s.discard();
                }
            })));
            (*stream_ptr).connect_record(None, Some(&buffer_attr), FlagSet::ADJUST_LATENCY)?;
        }
        self.mainloop.unlock();
        Ok(peak_level)
    }

}

impl Drop for PulseManager {
    fn drop(&mut self) {
        self.mainloop.lock();
        self.context.disconnect();
        self.mainloop.unlock();

        self.mainloop.stop();
    }
}

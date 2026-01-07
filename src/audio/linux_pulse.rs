use libpulse_binding as pulse;
use pulse::context::{Context, FlagSet, State};
use pulse::mainloop::threaded::Mainloop;
use pulse::sample::Format;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u8,
    pub format: Format,
}

pub struct PulseManager {
    // Wrap mainloop in Arc to allow shared access
    pub mainloop: Arc<std::sync::Mutex<Mainloop>>,
    pub context: Context,
    pub is_running: Arc<AtomicBool>,
}

impl PulseManager {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let mut mainloop = Mainloop::new().ok_or("Failed to create mainloop")?;
        let mut context = Context::new(&mainloop, "MinntyServer").ok_or("Context creation failed")?;

        context.connect(None, FlagSet::NOFLAGS, None)?;
        mainloop.start()?; 

        Ok(Self {
            mainloop: Arc::new(std::sync::Mutex::new(mainloop)),
            context,
            is_running: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn get_default_device_info(&self) -> Result<AudioDeviceInfo, Box<dyn std::error::Error>> {
        let mut ml = self.mainloop.lock().unwrap();

        // 1. Wait for Ready
        loop {
            ml.lock();
            let state = self.context.get_state();
            ml.unlock();

            match state {
                State::Ready => break,
                State::Failed | State::Terminated => return Err("PulseAudio connection failed".into()),
                _ => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }

        let (tx, rx) = std::sync::mpsc::channel();

        // 2. Get Server Info
        ml.lock();
        self.context.introspect().get_server_info(move |server_info| {
            let sink_name = server_info.default_sink_name.as_ref().map(|s| s.to_string());
            let _ = tx.send(sink_name);
        });
        ml.unlock();

        let default_sink_name = rx.recv()?.ok_or("No default sink found")?;

        // 3. Get Sink Details
        let (tx_info, rx_info) = std::sync::mpsc::channel();
        ml.lock();
        self.context.introspect().get_sink_info_by_name(&default_sink_name, move |res| {
            if let pulse::callbacks::ListResult::Item(i) = res {
                let info = AudioDeviceInfo {
                    name: i.name.as_ref().map(|s| s.to_string()).unwrap_or_default(),
                    sample_rate: i.sample_spec.rate,
                    channels: i.sample_spec.channels,
                    format: i.sample_spec.format,
                };
                let _ = tx_info.send(Some(info));
            } else {
                let _ = tx_info.send(None);
            }
        });
        ml.unlock();

        rx_info.recv()?.ok_or("Failed to get sink details".into())
    }
}

impl Drop for PulseManager {
    fn drop(&mut self) {
        // 1. Lock the mainloop to safely change state
        if let Ok(mut ml) = self.mainloop.lock() {
            // 2. Disconnect the context first
            self.context.disconnect();
            
            // 3. Stop the background thread
            ml.stop();
        }
        // Use \r to ensure text starts at the margin in raw mode
        println!("\rPulseAudio Mainloop shut down cleanly.");
    }
}
use libpulse_binding as pulse;
use pulse::context::{Context, State};
use pulse::mainloop::standard::Mainloop;
use pulse::sample::Format;
use std::rc::Rc;
use std::cell::RefCell;

#[derive(Debug)]
pub struct PulseAudioInfo {
    pub default_sink: String,
    pub sample_rate: u32,
    pub channels: u8,
    pub sample_format: Format,
}

pub fn probe_pulseaudio() -> Result<PulseAudioInfo, Box<dyn std::error::Error>> {
    // 1. Setup Mainloop and Context
    let mut mainloop = Mainloop::new().ok_or("Failed to create mainloop")?;
    let mut context = Context::new(&mainloop, "Minnty Probe").ok_or("Context creation failed")?;
    
    context.connect(None, pulse::context::FlagSet::NOFLAGS, None)?;

    // 2. Drive the mainloop until Context is Ready
    while context.get_state() != State::Ready {
        mainloop.iterate(false);
        match context.get_state() {
            State::Failed | State::Terminated => return Err("PulseAudio context failed".into()),
            _ => (),
        }
    }

    // 3. Fetch the Default Sink Name
    let sink_name_store = Rc::new(RefCell::new(None));
    let sink_name_cloned = Rc::clone(&sink_name_store);

    let op_server = context.introspect().get_server_info(move |info| {
        if let Some(name) = &info.default_sink_name {
            *sink_name_cloned.borrow_mut() = Some(name.to_string());
        }
    });

    // PulseAudio operations are async; we must "pump" the mainloop until the operation is done
    while op_server.get_state() == pulse::operation::State::Running {
        mainloop.iterate(false);
    }

    let default_sink_name = sink_name_store.borrow_mut().take()
        .ok_or("Could not determine default sink name")?;

    // 4. Fetch detailed info for that specific Sink
    let final_info = Rc::new(RefCell::new(None));
    let final_info_cloned = Rc::clone(&final_info);

    let op_sink = context.introspect().get_sink_info_by_name(&default_sink_name, move |info| {
        if let pulse::callbacks::ListResult::Item(i) = info {
            *final_info_cloned.borrow_mut() = Some(PulseAudioInfo {
                default_sink: i.name.as_ref().map(|s| s.to_string()).unwrap_or_default(),
                sample_rate: i.sample_spec.rate,
                channels: i.sample_spec.channels,
                sample_format: i.sample_spec.format,
            });
        }
    });

    while op_sink.get_state() == pulse::operation::State::Running {
        mainloop.iterate(false);
    }

    final_info.borrow_mut().take().ok_or("Failed to fetch detailed sink info".into())
}
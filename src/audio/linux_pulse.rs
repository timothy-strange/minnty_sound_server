use libpulse_binding as pulse;
use pulse::context::Context;
use pulse::mainloop::standard::Mainloop;

use pulse::sample::Format;

#[derive(Debug)]
pub struct PulseAudioInfo {
    pub default_sink: String,
    pub sample_rate: u32,
    pub channels: u8,
    pub sample_format: Format,
}

pub fn probe_pulseaudio() -> Result<PulseAudioInfo, Box<dyn std::error::Error>> {
    // create mainloop
    let mut mainloop = Mainloop::new().ok_or("Failed to create mainloop")?;

    // create context
    let mut context = Context::new(&mainloop, "Minnty Sound Server")
        .ok_or("Failed to create context")?;
    context.connect(None, pulse::context::FlagSet::NOFLAGS, None)?;

    // wait for context to be ready
    loop {
        match context.get_state() {
            pulse::context::State::Ready => break,
            pulse::context::State::Failed | pulse::context::State::Terminated => {
                return Err("PulseAudio context failed".into());
            }
            _ => {
                mainloop.iterate(false);
            }
        }
    }

    // Get default sink name
    let server_name = context.get_server().unwrap_or_else(|| "unknown".into());
    let default_sink_name = context.get_server().unwrap_or_else(|| "unknown".into());

    // Print what we got
    println!("Server: {}", server_name);
    println!("Default sink: {:?}", default_sink_name);

    // Note: full sink info retrieval is more complicated and requires async callbacks;
    // for minimal probe, we can skip sample rate/channels for now

    Ok(PulseAudioInfo {
        default_sink: default_sink_name,
        sample_rate: 48000,   // placeholder
        channels: 2,          // placeholder
        sample_format: Format::S16le, // placeholder
    })
}

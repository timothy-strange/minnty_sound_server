mod audio;

use audio::linux_pulse::probe_pulseaudio;

fn main() {
    println!("Minnty Sound Server starting");

    match probe_pulseaudio() {
        Ok(info) => {
            println!("Default sink: {}", info.default_sink);
            println!(
                "  Format: {} Hz, {} channels, {:?}",
                info.sample_rate, info.channels, info.sample_format
            );
        }
        Err(e) => {
            eprintln!("Error probing PulseAudio: {}", e);
            std::process::exit(1);
        }
    }
}

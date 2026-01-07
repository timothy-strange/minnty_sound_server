mod audio;

use audio::linux_pulse::{PulseManager, AudioDeviceInfo};
use std::sync::Arc;
use std::time::Duration;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

/// The central state of the application.
struct AppState {
    audio_manager: PulseManager,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Minnty Sound Server starting...");

    // 1. Initialize PulseAudio Manager
    let audio_manager = PulseManager::new()?;
    let state = Arc::new(AppState { audio_manager });

    // 2. Perform initial probe
    match state.audio_manager.get_default_device_info() {
        Ok(info) => {
            println!("--- System Audio Found ---");
            println!("Device:  {}", info.name);
            println!("Rate:    {}Hz", info.sample_rate);
            println!("Format:  {:?}", info.format);
        }
        Err(e) => {
            eprintln!("Initialization Error: {}", e);
            std::process::exit(1);
        }
    }

    println!("\nServer is running.");
    println!("Press [q] to quit.");

    // 3. Enter Raw Mode to capture keys without Enter
    enable_raw_mode()?;

    loop {
        // Poll for a keyboard event every 100ms
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                // Check if 'q' was pressed (ignoring key release events on Windows/Linux)
                if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('q') {
                    break;
                }
            }
        }
        
        // This is where you would check other state, like network status
    }

    // 4. Cleanup
    disable_raw_mode()?;
    drop(state);
    println!("\rQuitting.");
    Ok(())
}
mod audio;

use std::io::Write;
use audio::linux_pulse::PulseManager;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Minnty Sound Server starting");

    let mut pulse = PulseManager::new()?;

    // Start background capture and get a reference to the peak level
    let shared_peak = pulse.start_background_capture()?;

    println!("Capture started. Press 'q' to quit.");

    enable_raw_mode()?;

    loop {
        // 1. Handle Input
        if event::poll(Duration::from_millis(10))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('q') {
                    break;
                }
            }
        }

        // 2. Read the peak value from the background thread
        let bits = shared_peak.load(std::sync::atomic::Ordering::Relaxed);
        let peak = f32::from_bits(bits);

        // 3. Render Visuals
        let width = 40;
        let progress = (peak * width as f32) as usize;
        let bar = "█".repeat(progress.min(width));
        
        print!("\r\x1b[32mLevel: [{:<40}]\x1b[0m\x1b[K", bar);
        std::io::stdout().flush()?;
    }

    disable_raw_mode()?;
    println!("\nShutting down.");

    Ok(())
}
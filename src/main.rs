mod audio;
mod web;

use audio::linux_pulse::PulseManager;
use std::{net::SocketAddr};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut pulse = PulseManager::new()?;
    pulse.start_meters()?;

    let meters = pulse
        .export_meters()
        .into_iter()
        .map(|(name, peak)| web::MeterRef { name, peak })
        .collect::<Vec<_>>();

    let addr: SocketAddr = "127.0.0.1:3000".parse()?;

    // Keep pulse alive while the web server runs.
    web::run(addr, meters).await?;

    Ok(())
}

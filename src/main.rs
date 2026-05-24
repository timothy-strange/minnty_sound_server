mod audio;
mod control;
mod encode;
mod i18n;
mod launcher;
mod logging;
mod media_control;
mod testing;
mod transport;
mod web;

use audio::controller::{AudioController, AudioSource};
use control::http::StreamManager;
use control::messages::DEFAULT_STREAM_CONFIG;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::net::SocketAddr;
use std::sync::Arc;
use testing::net_impairment::NetImpairmentController;
use transport::udp::UdpServer;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let run_server_mode = std::env::args().any(|arg| arg == "--server");
    if !run_server_mode {
        return launcher::run_launcher();
    }

    run_server().await
}

async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let config = DEFAULT_STREAM_CONFIG;
    let session_id = rand::random::<u64>();

    let (audio_controller, meters) = AudioController::new()?;

    let udp_addr = SocketAddr::from(([0, 0, 0, 0], config.udp_port));
    let udp_server = Arc::new(UdpServer::bind(udp_addr, config, session_id).await?);
    let udp_listener = Arc::clone(&udp_server);
    tokio::spawn(async move {
        let _ = udp_listener.run_listener().await;
    });

    let audio_source: Arc<dyn AudioSource> = Arc::new(audio_controller);
    let impairment = Arc::new(NetImpairmentController::new());
    let stream_manager = Arc::new(StreamManager::new(
        audio_source,
        udp_server,
        config,
        Arc::clone(&impairment),
    ));
    let meters = meters
        .into_iter()
        .map(|(name, peak)| web::MeterRef { name, peak })
        .collect::<Vec<_>>();

    let addr: SocketAddr = "127.0.0.1:3000".parse()?;

    let _mdns = register_mdns(config);

    web::run(addr, meters, stream_manager).await?;

    Ok(())
}

fn register_mdns(config: control::messages::StreamConfig) -> Option<ServiceDaemon> {
    let mdns = ServiceDaemon::new().ok()?;
    let service_type = "_minnty._udp.local.";
    let ip = local_ip_address::local_ip().ok()?;
    let instance = format!("minnty-{}", ip).replace(':', "-");
    let hostname = format!("{}.local.", instance);
    let info = ServiceInfo::new(
        service_type,
        &instance,
        &hostname,
        ip,
        config.udp_port,
        None,
    )
    .ok()?;
    let _ = mdns.register(info);
    Some(mdns)
}

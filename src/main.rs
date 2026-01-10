mod audio;
mod control;
mod encode;
mod monitor;
mod transport;
mod web;

use audio::controller::{AudioController, AudioSource};
use control::http::StreamManager;
use control::messages::DEFAULT_STREAM_CONFIG;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use monitor::MonitorManager;
use std::net::SocketAddr;
use std::sync::Arc;
use transport::udp::UdpServer;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = DEFAULT_STREAM_CONFIG;

    let (audio_controller, meters) = AudioController::new()?;

    let udp_addr = SocketAddr::from(([0, 0, 0, 0], config.udp_port));
    let udp_server = Arc::new(UdpServer::bind(udp_addr, config).await?);
    let udp_listener = Arc::clone(&udp_server);
    tokio::spawn(async move {
        let _ = udp_listener.run_listener().await;
    });

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let audio_source: Arc<dyn AudioSource> = Arc::new(audio_controller);
    let stream_manager = Arc::new(StreamManager::new(
        audio_source,
        udp_server,
        config,
        shutdown_tx,
    ));
    let monitor_manager = Arc::new(MonitorManager::new(config));

    let meters = meters
        .into_iter()
        .map(|(name, peak)| web::MeterRef { name, peak })
        .collect::<Vec<_>>();

    let addr: SocketAddr = "127.0.0.1:3000".parse()?;

    let _mdns = register_mdns(config);

    web::run(addr, meters, stream_manager, monitor_manager, shutdown_rx).await?;

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

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

// This marks the main entry point as asynchronous, meaning it can pause and wait
// for things like network connections without freezing the whole program.
// Tokio will generate a normal main function for us, start its runtime, and then
// run this async function on its worker threads.
// In Rust, the `async` keyword turns a function into something that can be
// suspended and resumed, which is very handy for network servers.
// The square‑bracket attribute is processed at compile time, so this behavior is
// baked into the program before it ever runs.
#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `config` has type `StreamConfig`. It is a small bundle of settings like
    // the UDP port, sample rate, and frame size. The value is copied where needed,
    // so it is safe and cheap to pass around.
    // Rust structs like this are just organized groups of fields, similar to a
    // form with labeled boxes. Copying a small struct is often faster than
    // allocating memory, so this is a common pattern.
    let config = DEFAULT_STREAM_CONFIG;

    // `audio_controller` has type `AudioController`, which is a small object that
    // knows how to send commands to the dedicated audio thread. `meters` has type
    // `MeterExport`, which is a list of (name, value) pairs used by the UI to show
    // audio levels. The `?` symbol means: if initialization fails, return the error
    // from `main` immediately.
    // The `?` operator is a shorthand for error handling; it saves you from writing
    // a long `match` every time something might fail. It only works in functions
    // that return a `Result`, which `main` does here.
    let (audio_controller, meters) = AudioController::new()?;

    // `udp_addr` has type `SocketAddr`. It describes an IP address and port in a
    // single value, and here it means "listen on all network interfaces" on the
    // configured UDP port. This is where clients will send control packets.
    // The `SocketAddr::from` call is just a convenient way to build the struct
    // without writing out every field by hand.
    let udp_addr = SocketAddr::from(([0, 0, 0, 0], config.udp_port));
    // `udp_server` has type `Arc<UdpServer>`. The `Arc` is a thread‑safe shared
    // pointer, so multiple tasks can hold it at the same time. The `.await` waits
    // for the async bind operation to complete, and the `?` propagates any error.
    // The `Arc` keeps a reference count, and the data stays alive until the last
    // clone is dropped. The `.await` keyword is what lets async functions pause
    // without blocking the whole thread.
    let udp_server = Arc::new(UdpServer::bind(udp_addr, config).await?);
    // `udp_listener` has type `Arc<UdpServer>` too. This is just another shared
    // handle to the same server, created so the background task can own it.
    // Cloning an `Arc` does not duplicate the server, it only makes another handle
    // to the same server in memory.
    let udp_listener = Arc::clone(&udp_server);
    // This launches a background task that waits for UDP control packets. It runs
    // in the Tokio runtime so the main function can keep doing other setup work.
    // Spawning a task is like saying “please handle this in the background while
    // I continue with other responsibilities.”
    tokio::spawn(async move {
        // The result is ignored here because the listener is meant to run forever.
        // The underscore `_` is a Rust convention meaning “I don’t care about this
        // value,” which keeps the compiler from complaining.
        let _ = udp_listener.run_listener().await;
    });

    // `shutdown_tx` and `shutdown_rx` are two ends of a watch channel carrying a
    // boolean value. The sender can broadcast "please shut down" to anything that
    // holds the receiver, and the receiver can be awaited without blocking.
    // A watch channel always holds the latest value, so new listeners immediately
    // see the most recent signal. It’s a simple way to announce state changes.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    // `audio_source` has type `Arc<dyn AudioSource>`. This is a shared pointer to
    // something that implements the `AudioSource` trait. It allows the rest of the
    // program to request audio without knowing the concrete type.
    // The `dyn` keyword means “dynamic dispatch,” which is Rust’s way of letting
    // different implementations behave the same at runtime.
    let audio_source: Arc<dyn AudioSource> = Arc::new(audio_controller);
    // `stream_manager` has type `Arc<StreamManager>`. This object coordinates the
    // audio capture, encoding, and UDP sending. It is wrapped in `Arc` because many
    // web handlers will need to use it at the same time.
    // This is a typical pattern for shared state in async servers: wrap it in `Arc`
    // and pass clones to each task that needs access.
    let stream_manager = Arc::new(StreamManager::new(
        audio_source,
        udp_server,
        config,
        shutdown_tx,
    ));
    // `monitor_manager` has type `Arc<MonitorManager>`. It runs a local monitoring
    // client that listens to the UDP stream and calculates peak levels.
    // Keeping this separate from streaming logic makes the program easier to
    // understand and test, because each component has one job.
    let monitor_manager = Arc::new(MonitorManager::new(config));

    // `meters` is rebuilt into a `Vec<MeterRef>` so the web module can use it. Each
    // `MeterRef` holds a name and a shared atomic peak value.
    // The `map` call transforms each item in the list, and `collect` gathers them
    // into a new vector. This is a very common pattern in Rust for data transforms.
    let meters = meters
        .into_iter()
        .map(|(name, peak)| web::MeterRef { name, peak })
        .collect::<Vec<_>>();

    // `addr` has type `SocketAddr` and is where the HTTP server will listen. The
    // `parse()` call converts a string like "127.0.0.1:3000" into a structured type.
    // In Rust, parsing returns a `Result`, which is why `?` is used here to handle
    // any invalid input.
    let addr: SocketAddr = "127.0.0.1:3000".parse()?;

    // `_mdns` has type `Option<ServiceDaemon>`. The underscore name indicates the
    // value is kept alive but not otherwise used. Holding it prevents mDNS from
    // shutting down while the program is running.
    // This is a subtle ownership detail: if `_mdns` were dropped, the service would
    // stop being advertised. Keeping it in scope is enough to keep it alive.
    let _mdns = register_mdns(config);

    // This starts the web server, serves the UI, and waits until shutdown. The
    // `.await` means `main` pauses here until the web server finishes.
    // When an async function awaits, the thread is free to do other work while
    // waiting for I/O, which is much more efficient than blocking.
    web::run(addr, meters, stream_manager, monitor_manager, shutdown_rx).await?;

    // Returning `Ok(())` tells the operating system the program ended normally.
    // The empty tuple `()` is Rust’s way of saying “no meaningful value.”
    Ok(())
}

// This helper announces the server on the local network via mDNS, so clients can
// discover it without typing an IP address manually.
// It returns `None` if mDNS setup fails, because discovery is optional.
fn register_mdns(config: control::messages::StreamConfig) -> Option<ServiceDaemon> {
    // `mdns` has type `ServiceDaemon`. The `ok()?` pattern converts any error into
    // `None`, which simply means "skip mDNS" instead of crashing.
    // The `?` here works with `Option` too: it returns `None` early if creation fails.
    let mdns = ServiceDaemon::new().ok()?;
    // `service_type` is the service name clients look for on the local network.
    // This naming convention is how mDNS distinguishes different kinds of services.
    let service_type = "_minnty._udp.local.";
    // `ip` has type `IpAddr` and holds the machine's local network address.
    // This is the address other devices on the same network can use to reach us.
    let ip = local_ip_address::local_ip().ok()?;
    // `instance` is a human‑friendly identifier that makes each server unique.
    // The `replace` call changes `:` characters so the name is valid for mDNS.
    let instance = format!("minnty-{}", ip).replace(':', "-");
    // `hostname` is the local mDNS host name built from the instance name.
    // It ends in `.local.` which is the standard suffix for mDNS hostnames.
    let hostname = format!("{}.local.", instance);
    // `info` has type `ServiceInfo` and stores the details that mDNS will announce.
    // This includes the service type, hostname, IP address, and port number.
    let info = ServiceInfo::new(
        service_type,
        &instance,
        &hostname,
        ip,
        config.udp_port,
        None,
    )
    .ok()?;
    // This sends the announcement. The result is ignored because mDNS is optional.
    // If this fails, the server still works; it just won’t be auto‑discoverable.
    let _ = mdns.register(info);
    // Returning the daemon keeps mDNS alive for as long as the caller holds it.
    // In Rust, values are dropped automatically when they go out of scope.
    Some(mdns)
}

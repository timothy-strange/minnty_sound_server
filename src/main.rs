mod audio;

use audio::linux_pulse::PulseManager;
use axum::{
    extract::State,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use std::sync::{atomic::{AtomicBool, AtomicU32, Ordering}, Arc};
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot};

#[derive(Clone)]
struct AppState {
    is_running: Arc<AtomicBool>,
    peak_level: Arc<AtomicU32>,
    device_cmd_tx: mpsc::Sender<oneshot::Sender<Vec<String>>>,
}

#[derive(Serialize)]
struct StatusResponse {
    running: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let is_running = Arc::new(AtomicBool::new(false));
    let peak_level = Arc::new(AtomicU32::new(0));
    
    // Channel for the web server to request device lists from the Pulse thread
    let (tx, mut rx) = mpsc::channel::<oneshot::Sender<Vec<String>>>(1);

    let state = AppState { 
        is_running: is_running.clone(),
        peak_level: peak_level.clone(),
        device_cmd_tx: tx,
    };

    // Clone for the Pulse thread
    let thread_is_running = is_running.clone();
    let thread_peak_level = peak_level.clone();

    // Spawn a dedicated thread to OWN the PulseManager
    std::thread::spawn(move || {
        // PulseManager is created HERE, so it never leaves this thread
        let mut pulse = match PulseManager::new() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Failed to init PulseAudio: {}", e);
                return;
            }
        };

        // Start capture logic inside this thread
        // Note: We ignore the return here because we are writing to thread_peak_level inside linux_pulse
        let _ = pulse.start_background_capture(thread_is_running, thread_peak_level);

        // Wait for requests from the web handlers
        while let Some(reply_tx) = rx.blocking_recv() {
            let sinks = pulse.get_all_sinks().unwrap_or_else(|_| vec!["Error".to_string()]);
            let _ = reply_tx.send(sinks);
        }
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/status", get(get_status))
        .route("/toggle", post(toggle_server))
        .route("/devices", get(get_devices))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let _ = open::that(format!("http://{}", addr));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn index_handler() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

async fn get_status(State(state): State<AppState>) -> Json<StatusResponse> {
    Json(StatusResponse { running: state.is_running.load(Ordering::Relaxed) })
}

async fn toggle_server(State(state): State<AppState>) -> impl IntoResponse {
    let current = state.is_running.load(Ordering::Relaxed);
    state.is_running.store(!current, Ordering::Relaxed);
    Json(StatusResponse { running: !current })
}

async fn get_devices(State(state): State<AppState>) -> Json<Vec<String>> {
    let (tx, rx) = oneshot::channel();
    let _ = state.device_cmd_tx.send(tx).await;
    match rx.await {
        Ok(sinks) => Json(sinks),
        Err(_) => Json(vec!["Channel error".to_string()]),
    }
}
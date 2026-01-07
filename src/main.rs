mod audio;

use audio::linux_pulse::PulseManager;
use axum::{
    extract::State,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
use std::net::SocketAddr;

#[derive(Clone)]
struct AppState {
    is_running: Arc<AtomicBool>,
}

#[derive(Serialize)]
struct StatusResponse {
    running: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut pulse = PulseManager::new()?;
    let is_running = Arc::new(AtomicBool::new(false));
    let state = AppState { is_running: is_running.clone() };

    // We pass the atomic bool to the capture logic
    // Note: We'll modify start_background_capture to respect this bool next
    let _shared_peak = pulse.start_background_capture(is_running.clone())?;

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/status", get(get_status))
        .route("/toggle", post(toggle_server))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let url = format!("http://{}", addr);
    
    println!("Minnty Sound Server UI running at http://{}", addr);

    // This opens the default browser automatically
    let _ = open::that(url);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn index_handler() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

async fn get_status(State(state): State<AppState>) -> Json<StatusResponse> {
    Json(StatusResponse {
        running: state.is_running.load(Ordering::Relaxed),
    })
}

async fn toggle_server(State(state): State<AppState>) -> impl IntoResponse {
    let current = state.is_running.load(Ordering::Relaxed);
    state.is_running.store(!current, Ordering::Relaxed);
    Json(StatusResponse { running: !current })
}
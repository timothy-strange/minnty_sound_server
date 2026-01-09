const INDEX_HTML: &str = include_str!("index.html");

use axum::{
    extract::State,
    response::IntoResponse,
    response::Html,
    routing::get,
    Json, Router,
};
use serde::Serialize;
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

#[derive(Clone)]
pub struct MeterRef {
    pub name: String,
    pub peak: Arc<AtomicU32>,
}

#[derive(Clone)]
struct AppState {
    meters: Arc<Vec<MeterRef>>,
}

#[derive(Serialize)]
struct Level {
    name: String,
    peak: f32,
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

pub async fn run(
    addr: SocketAddr,
    meters: Vec<MeterRef>,
) -> Result<(), Box<dyn std::error::Error>> {
    let state = AppState {
        meters: Arc::new(meters),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/levels", get(api_levels))
        .with_state(state);

    let url = format!("http://{}/", addr);
    let _ = open::that(&url);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn api_levels(State(state): State<AppState>) -> impl IntoResponse {
    let levels = state
        .meters
        .iter()
        .map(|m| Level {
            name: m.name.clone(),
            peak: f32::from_bits(m.peak.load(Ordering::Relaxed)),
        })
        .collect::<Vec<_>>();

    Json(levels)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

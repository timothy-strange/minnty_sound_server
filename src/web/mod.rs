const INDEX_HTML: &str = include_str!("index.html");

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::Html,
    response::IntoResponse,
    routing::{get, post},
};
use serde::Serialize;
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use crate::control::http::{StartStreamRequest, StreamManager, StreamStatusResponse};
use crate::i18n;

#[derive(Clone)]
pub struct MeterRef {
    pub name: String,
    pub peak: Arc<AtomicU32>,
}

#[derive(Clone)]
struct AppState {
    meters: Arc<Vec<MeterRef>>,
    stream: Arc<StreamManager>,
}

#[derive(Serialize)]
struct Level {
    name: String,
    peak: f32,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

pub async fn run(
    addr: SocketAddr,
    meters: Vec<MeterRef>,
    stream: Arc<StreamManager>,
) -> Result<(), Box<dyn std::error::Error>> {
    let state = AppState {
        meters: Arc::new(meters),
        stream,
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/i18n", get(api_i18n))
        .route("/api/levels", get(api_levels))
        .route("/api/stream/start", post(start_stream))
        .route("/api/stream/stop", post(stop_stream))
        .route("/api/stream/status", get(stream_status))
        .with_state(state);

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

async fn api_i18n() -> impl IntoResponse {
    let payload = serde_json::json!({
        "locale": i18n::locale(),
        "strings": i18n::strings(),
    });
    Json(payload)
}

async fn start_stream(
    State(state): State<AppState>,
    Json(payload): Json<StartStreamRequest>,
) -> Result<(StatusCode, Json<StreamStatusResponse>), (StatusCode, Json<ErrorResponse>)> {
    match state.stream.start(payload.sink).await {
        Ok(()) => {
            let status = state.stream.status().await;
            Ok((StatusCode::OK, Json(status)))
        }
        Err(err) => Err(error_response(StatusCode::BAD_REQUEST, err)),
    }
}

async fn stop_stream(
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<StreamStatusResponse>), (StatusCode, Json<ErrorResponse>)> {
    match state.stream.stop().await {
        Ok(()) => {
            let status = state.stream.status().await;
            Ok((StatusCode::OK, Json(status)))
        }
        Err(err) => Err(error_response(StatusCode::BAD_REQUEST, err)),
    }
}

async fn stream_status(State(state): State<AppState>) -> impl IntoResponse {
    let status: StreamStatusResponse = state.stream.status().await;
    (StatusCode::OK, Json(status))
}

fn error_response(status: StatusCode, message: String) -> (StatusCode, Json<ErrorResponse>) {
    (status, Json(ErrorResponse { error: message }))
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

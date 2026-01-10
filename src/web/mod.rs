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
use tokio::sync::watch;

use crate::control::http::{StartStreamRequest, StreamManager, StreamStatusResponse};
use crate::monitor::{MonitorManager, MonitorStatus};

#[derive(Clone)]
pub struct MeterRef {
    pub name: String,
    pub peak: Arc<AtomicU32>,
}

#[derive(Clone)]
struct AppState {
    meters: Arc<Vec<MeterRef>>,
    stream: Arc<StreamManager>,
    monitor: Arc<MonitorManager>,
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

#[derive(Serialize)]
struct MonitorResponse {
    active: bool,
    peak: f32,
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

pub async fn run(
    addr: SocketAddr,
    meters: Vec<MeterRef>,
    stream: Arc<StreamManager>,
    monitor: Arc<MonitorManager>,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let state = AppState {
        meters: Arc::new(meters),
        stream,
        monitor,
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/levels", get(api_levels))
        .route("/api/stream/start", post(start_stream))
        .route("/api/stream/stop", post(stop_stream))
        .route("/api/stream/status", get(stream_status))
        .route("/api/monitor/start", post(start_monitor))
        .route("/api/monitor/stop", post(stop_monitor))
        .route("/api/monitor/level", get(monitor_level))
        .route("/api/shutdown", post(shutdown))
        .with_state(state);

    let url = format!("http://{}/", addr);
    let _ = open::that(&url);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_rx))
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
            state.monitor.stop().await;
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

async fn start_monitor(
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<MonitorResponse>), (StatusCode, Json<ErrorResponse>)> {
    let stream_status = state.stream.status().await;
    if stream_status.sink.is_none() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Stream is not running".to_string(),
        ));
    }

    match state.monitor.start().await {
        Ok(()) => {
            let status = state.monitor.status().await;
            Ok((StatusCode::OK, Json(to_monitor_response(status))))
        }
        Err(err) => Err(error_response(StatusCode::BAD_REQUEST, err)),
    }
}

async fn stop_monitor(State(state): State<AppState>) -> impl IntoResponse {
    state.monitor.stop().await;
    let status = state.monitor.status().await;
    (StatusCode::OK, Json(to_monitor_response(status)))
}

async fn monitor_level(State(state): State<AppState>) -> impl IntoResponse {
    let status = state.monitor.status().await;
    (StatusCode::OK, Json(to_monitor_response(status)))
}

async fn shutdown(State(state): State<AppState>) -> impl IntoResponse {
    state.monitor.stop().await;
    state.stream.shutdown();
    (StatusCode::OK, Json(()))
}

fn error_response(status: StatusCode, message: String) -> (StatusCode, Json<ErrorResponse>) {
    (status, Json(ErrorResponse { error: message }))
}

fn to_monitor_response(status: MonitorStatus) -> MonitorResponse {
    MonitorResponse {
        active: status.active,
        peak: status.peak,
    }
}

async fn shutdown_signal(mut shutdown_rx: watch::Receiver<bool>) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = shutdown_rx.changed() => {},
    }
}

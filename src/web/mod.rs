const INDEX_HTML: &str = include_str!("index.html");

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::Html,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use crate::control::http::{
    CALIBRATION_STREAM_NAME, CalibrationMode, StartStreamRequest, StreamManager,
    StreamStatusResponse,
};
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

#[derive(Serialize)]
struct FeatureFlagsResponse {
    version: &'static str,
    net_impairment_ui: bool,
}

#[derive(Deserialize)]
struct I18nQuery {
    locale: Option<String>,
}

#[cfg(feature = "net_impairment_ui")]
#[derive(Deserialize)]
struct TriggerGapRequest {
    delay_ms: u64,
}

#[derive(Serialize)]
struct SettingsResponse {
    frame_duration_ms: u32,
    change_server_volume_from_clients: bool,
    calibration_mode: String,
}

#[derive(Deserialize)]
struct SetSettingsRequest {
    frame_duration_ms: Option<u32>,
    change_server_volume_from_clients: Option<bool>,
    calibration_mode: Option<String>,
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
        .route("/api/features", get(api_features))
        .route("/api/i18n", get(api_i18n))
        .route("/api/levels", get(api_levels))
        .route("/api/stream/start", post(start_stream))
        .route("/api/stream/stop", post(stop_stream))
        .route("/api/stream/status", get(stream_status))
        .route("/api/settings", get(get_settings).post(set_settings));

    #[cfg(feature = "net_impairment_ui")]
    let app = app.route(
        "/api/test/network-gap-once",
        post(api_trigger_network_gap_once),
    );

    let app = app.with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn api_levels(State(state): State<AppState>) -> impl IntoResponse {
    let mut levels = state
        .meters
        .iter()
        .map(|m| Level {
            name: m.name.clone(),
            peak: f32::from_bits(m.peak.load(Ordering::Relaxed)),
        })
        .collect::<Vec<_>>();

    levels.push(Level {
        name: CALIBRATION_STREAM_NAME.to_string(),
        peak: 0.0,
    });

    Json(levels)
}

async fn api_i18n(Query(query): Query<I18nQuery>) -> impl IntoResponse {
    let (locale, strings) = i18n::strings_for(query.locale.as_deref());
    let payload = serde_json::json!({
        "locale": locale,
        "dir": i18n::direction(locale),
        "available_locales": i18n::AVAILABLE_LOCALES,
        "strings": strings,
    });
    Json(payload)
}

async fn api_features() -> impl IntoResponse {
    Json(FeatureFlagsResponse {
        version: env!("CARGO_PKG_VERSION"),
        net_impairment_ui: cfg!(feature = "net_impairment_ui"),
    })
}

#[cfg(feature = "net_impairment_ui")]
async fn api_trigger_network_gap_once(
    State(state): State<AppState>,
    Json(payload): Json<TriggerGapRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<ErrorResponse>)> {
    match state.stream.trigger_test_gap_once_ms(payload.delay_ms) {
        Ok(()) => Ok((
            StatusCode::OK,
            Json(serde_json::json!({ "ok": true, "delay_ms": payload.delay_ms })),
        )),
        Err(err) => Err(error_response(StatusCode::BAD_REQUEST, err)),
    }
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

async fn get_settings(State(state): State<AppState>) -> impl IntoResponse {
    Json(SettingsResponse {
        frame_duration_ms: state.stream.get_frame_duration_ms(),
        change_server_volume_from_clients: state.stream.change_server_volume_from_clients(),
        calibration_mode: state.stream.get_calibration_mode().as_str().to_string(),
    })
}

async fn set_settings(
    State(state): State<AppState>,
    Json(payload): Json<SetSettingsRequest>,
) -> Result<(StatusCode, Json<SettingsResponse>), (StatusCode, Json<ErrorResponse>)> {
    if let Some(frame_duration_ms) = payload.frame_duration_ms {
        if let Err(err) = state.stream.set_frame_duration_ms(frame_duration_ms) {
            return Err(error_response(StatusCode::BAD_REQUEST, err));
        }
    }
    if let Some(enabled) = payload.change_server_volume_from_clients {
        state.stream.set_change_server_volume_from_clients(enabled);
    }
    if let Some(mode) = payload.calibration_mode {
        let mode = CalibrationMode::parse(&mode).ok_or_else(|| {
            error_response(
                StatusCode::BAD_REQUEST,
                "Unsupported calibration mode".to_string(),
            )
        })?;
        state.stream.set_calibration_mode(mode);
    }
    Ok((
        StatusCode::OK,
        Json(SettingsResponse {
            frame_duration_ms: state.stream.get_frame_duration_ms(),
            change_server_volume_from_clients: state.stream.change_server_volume_from_clients(),
            calibration_mode: state.stream.get_calibration_mode().as_str().to_string(),
        }),
    ))
}

fn error_response(status: StatusCode, message: String) -> (StatusCode, Json<ErrorResponse>) {
    (status, Json(ErrorResponse { error: message }))
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

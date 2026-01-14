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

// `MeterRef` is a small piece of shared data used by the web layer. It lets the
// UI read peak values without owning or copying them.
// The `derive(Clone)` means copies are cheap because only the `Arc` is cloned.
// Cloning here does not duplicate the underlying meter data.
#[derive(Clone)]
pub struct MeterRef {
    // `name` has type `String`. It is displayed in the UI so the user knows which
    // audio sink the meter corresponds to.
    // The string is owned here so it stays alive as long as the meter reference.
    // This avoids dangling references in the UI.
    pub name: String,
    // `peak` has type `Arc<AtomicU32>`. The `Arc` means the value is shared, and
    // the `AtomicU32` means it can be updated safely from another thread.
    // The float is stored as bits because atomic floats are not available in Rust.
    // The UI converts the bits back into `f32` for display.
    pub peak: Arc<AtomicU32>,
}

// `AppState` collects everything the HTTP handlers need access to.
// It is stored inside Axum’s state system so each handler can retrieve it.
// This keeps handler signatures simple and consistent.
#[derive(Clone)]
struct AppState {
    // `meters` has type `Arc<Vec<MeterRef>>`. This list is shared so every handler
    // can read meter values without copying the whole list each time.
    // Wrapping the vector in `Arc` means handlers share the same vector in memory.
    // This is efficient for read‑only data.
    meters: Arc<Vec<MeterRef>>,
    // `stream` has type `Arc<StreamManager>`. The handlers use this to start or
    // stop the audio stream.
    // Using `Arc` here means each handler can hold a handle safely.
    // This is important because handlers run concurrently.
    stream: Arc<StreamManager>,
    // `monitor` has type `Arc<MonitorManager>`. The handlers use this to start or
    // stop the local monitoring client.
    // Keeping monitor logic separate makes the API easier to understand.
    // It also keeps streaming and monitoring concerns separated.
    monitor: Arc<MonitorManager>,
}

// `Level` is the JSON structure returned by the /api/levels endpoint.
// It’s designed to be easy for a UI to render.
// Each element corresponds to one audio sink.
#[derive(Serialize)]
struct Level {
    // `name` is the label shown in the UI for a meter.
    // This is a plain string so front‑end code can display it directly.
    // It comes from the sink name list collected at startup.
    name: String,
    // `peak` is the current peak level, expressed as a floating‑point number.
    // This value is normalized between 0.0 and 1.0 for easy display.
    // The UI can multiply by 100 to show a percentage if desired.
    peak: f32,
}

// `ErrorResponse` is the JSON structure used for reporting errors to the UI.
// It provides a single `error` field so front‑end code is simple.
// A consistent error shape makes the UI easier to code.
#[derive(Serialize)]
struct ErrorResponse {
    // `error` is a friendly message that can be shown to the user.
    // It is not meant for machine parsing, just human reading.
    // Keeping it as a String keeps things flexible.
    error: String,
}

// `MonitorResponse` is the JSON structure for monitor status endpoints.
// It is similar to `MonitorStatus` but tailored for web responses.
// Keeping a separate type makes serialization explicit.
#[derive(Serialize)]
struct MonitorResponse {
    // `active` is true when monitoring is running.
    // The UI can use this to show the correct button state.
    // It is a straightforward boolean value.
    active: bool,
    // `peak` is the latest monitored peak value.
    // This is the number the UI can render as a meter.
    // It is updated by the monitor task in the background.
    peak: f32,
}

// This handler simply returns the embedded HTML page as the response body.
// It is the root route for the UI.
// `Html` is an Axum response wrapper that sets the right content type.
async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

// This function builds the router, starts the HTTP server, and waits until the
// server is told to shut down.
// It is the main entry point for the web layer.
// The returned `Result` lets startup errors propagate cleanly.
pub async fn run(
    // `addr` has type `SocketAddr` and tells the server which IP and port to use.
    // This is where the web browser connects.
    // Using a structured address avoids string parsing later.
    addr: SocketAddr,
    // `meters` has type `Vec<MeterRef>` and is converted into shared state below.
    // This vector is produced during startup and represents available sinks.
    // It is read‑only for handlers, so sharing is safe.
    meters: Vec<MeterRef>,
    // `stream` has type `Arc<StreamManager>` and is shared across request handlers.
    // The manager owns the core streaming logic.
    // Sharing avoids needing to recreate it per request.
    stream: Arc<StreamManager>,
    // `monitor` has type `Arc<MonitorManager>` and is shared across handlers.
    // It controls the background monitor task.
    // Keeping this in state allows multiple handlers to coordinate.
    monitor: Arc<MonitorManager>,
    // `shutdown_rx` has type `watch::Receiver<bool>` and is used for graceful stop.
    // The server will stop when this channel changes or Ctrl+C is pressed.
    // This is a cooperative shutdown mechanism.
    shutdown_rx: watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error>> {
    // `state` has type `AppState` and bundles all shared data together.
    // The `Arc` fields inside keep the data alive across requests.
    // This is the main way Axum handlers access shared resources.
    let state = AppState {
        meters: Arc::new(meters),
        stream,
        monitor,
    };

    // `app` is a router. Each `.route` line maps a URL to a function.
    // Axum uses these routes to dispatch incoming HTTP requests.
    // The `.with_state` call attaches shared state to all handlers.
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

    // `url` is a String that will be opened in the user's browser.
    // Opening the browser is a convenience so the user sees the UI immediately.
    // If opening fails, the server still continues.
    let url = format!("http://{}/", addr);
    let _ = open::that(&url);

    // `listener` is a TCP listener bound to the HTTP address. The `.await` means
    // this function pauses until the OS finishes setting up the socket.
    // Binding can fail if the port is already in use.
    // If that happens, the error is returned to the caller.
    let listener = tokio::net::TcpListener::bind(addr).await?;
    // This starts the server. It will keep running until the shutdown signal fires.
    // The graceful shutdown waits for in‑flight requests to finish.
    // This is nicer than abruptly killing the server.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_rx))
        .await?;

    Ok(())
}

// This handler returns a list of current peak levels as JSON.
// It is called by the UI to update the meter display.
// The response is a JSON array of `Level` objects.
async fn api_levels(State(state): State<AppState>) -> impl IntoResponse {
    // `levels` has type `Vec<Level>` and is built by reading the shared meters.
    // Each meter’s atomic value is converted from bits back to `f32`.
    // This is safe because the value was originally stored as float bits.
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

// This handler starts streaming using the sink named in the JSON body.
// The JSON body is parsed into `StartStreamRequest` automatically.
// Errors are returned as JSON with a status code.
async fn start_stream(
    State(state): State<AppState>,
    Json(payload): Json<StartStreamRequest>,
) -> Result<(StatusCode, Json<StreamStatusResponse>), (StatusCode, Json<ErrorResponse>)> {
    match state.stream.start(payload.sink).await {
        Ok(()) => {
            // `status` has type `StreamStatusResponse` and shows the updated state.
            // Returning the status lets the UI refresh its display immediately.
            // This avoids needing a second API call.
            let status = state.stream.status().await;
            Ok((StatusCode::OK, Json(status)))
        }
        Err(err) => Err(error_response(StatusCode::BAD_REQUEST, err)),
    }
}

// This handler stops streaming and returns the new status.
// It also stops the monitor task because monitoring requires streaming.
// Returning the status gives the UI immediate feedback.
async fn stop_stream(
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<StreamStatusResponse>), (StatusCode, Json<ErrorResponse>)> {
    match state.stream.stop().await {
        Ok(()) => {
            state.monitor.stop().await;
            // `status` has type `StreamStatusResponse` after stopping.
            // This tells the UI that streaming is now inactive.
            // The UI can use this to disable certain buttons.
            let status = state.stream.status().await;
            Ok((StatusCode::OK, Json(status)))
        }
        Err(err) => Err(error_response(StatusCode::BAD_REQUEST, err)),
    }
}

// This handler returns whether streaming is running.
// It is a lightweight status endpoint for the UI.
// The response uses the same `StreamStatusResponse` struct.
async fn stream_status(State(state): State<AppState>) -> impl IntoResponse {
    // `status` has type `StreamStatusResponse` and is returned as JSON.
    // The UI can poll this if it wants to stay in sync.
    // It is a quick read because it only locks state briefly.
    let status: StreamStatusResponse = state.stream.status().await;
    (StatusCode::OK, Json(status))
}

// This handler starts the monitor task if streaming is active.
// The monitor depends on streaming because it listens to the UDP stream.
// If streaming is not active, it returns an error.
async fn start_monitor(
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<MonitorResponse>), (StatusCode, Json<ErrorResponse>)> {
    // `stream_status` has type `StreamStatusResponse` and tells us if a stream exists.
    // If there is no sink, monitoring would have nothing to listen to.
    // That’s why this guard is in place.
    let stream_status = state.stream.status().await;
    if stream_status.sink.is_none() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Stream is not running".to_string(),
        ));
    }

    match state.monitor.start().await {
        Ok(()) => {
            // `status` has type `MonitorStatus` and is converted to JSON.
            // Returning the status helps the UI update immediately.
            // It avoids a separate status fetch call.
            let status = state.monitor.status().await;
            Ok((StatusCode::OK, Json(to_monitor_response(status))))
        }
        Err(err) => Err(error_response(StatusCode::BAD_REQUEST, err)),
    }
}

// This handler stops the monitor task and returns the last known value.
// It can be called even if monitoring is already stopped.
// The response is still useful for resetting UI state.
async fn stop_monitor(State(state): State<AppState>) -> impl IntoResponse {
    state.monitor.stop().await;
    // `status` has type `MonitorStatus` after stopping.
    // It typically reports `active: false` and a peak of 0.
    // The UI can use this to clear the meter display.
    let status = state.monitor.status().await;
    (StatusCode::OK, Json(to_monitor_response(status)))
}

// This handler returns the current monitor value without changing anything.
// It is useful when the UI wants to refresh the peak display.
// It does not start or stop the monitor task.
async fn monitor_level(State(state): State<AppState>) -> impl IntoResponse {
    let status = state.monitor.status().await;
    (StatusCode::OK, Json(to_monitor_response(status)))
}

// This handler stops monitoring and streaming, then signals the server to quit.
// It is effectively the “shutdown” endpoint.
// The UI calls this when the user clicks a shutdown button.
async fn shutdown(State(state): State<AppState>) -> impl IntoResponse {
    state.monitor.stop().await;
    state.stream.shutdown();
    (StatusCode::OK, Json(()))
}

// This helper builds a consistent error response for all handlers.
// Consistency makes it easier for the UI to display errors uniformly.
// The status code is included so the HTTP client knows it failed.
fn error_response(status: StatusCode, message: String) -> (StatusCode, Json<ErrorResponse>) {
    (status, Json(ErrorResponse { error: message }))
}

// This helper converts `MonitorStatus` into the JSON `MonitorResponse`.
// It is a tiny adapter function used by multiple handlers.
// Keeping it separate avoids repeated code.
fn to_monitor_response(status: MonitorStatus) -> MonitorResponse {
    MonitorResponse {
        active: status.active,
        peak: status.peak,
    }
}

// This async function waits until either Ctrl+C is pressed or the shutdown
// channel changes, and then it allows the server to exit gracefully.
// Using `tokio::select!` lets it wait on multiple signals at once.
// This provides a clean shutdown path for both manual and programmatic stops.
async fn shutdown_signal(mut shutdown_rx: watch::Receiver<bool>) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = shutdown_rx.changed() => {},
    }
}

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use posthog_rs::{CaptureMode, Client, ClientOptionsBuilder, Event};

#[derive(Clone)]
struct AppState {
    inner: Arc<Mutex<AdapterState>>,
    capture_mode: CaptureMode,
}

struct AdapterState {
    client: Option<Client>,
    total_events_captured: u64,
    total_events_sent: u64,
    total_retries: u64,
    last_error: Option<String>,
    requests_made: Vec<RequestRecord>,
    pending_events: i64,
}

impl Default for AdapterState {
    fn default() -> Self {
        Self {
            client: None,
            total_events_captured: 0,
            total_events_sent: 0,
            total_retries: 0,
            last_error: None,
            requests_made: Vec::new(),
            pending_events: 0,
        }
    }
}

#[derive(Serialize, Clone)]
struct RequestRecord {
    timestamp_ms: u64,
    status_code: u16,
    retry_attempt: u32,
    event_count: usize,
    uuid_list: Vec<String>,
}

#[derive(Deserialize)]
struct InitRequest {
    api_key: String,
    host: String,
    #[allow(dead_code)]
    #[serde(default)]
    flush_at: Option<u32>,
    #[allow(dead_code)]
    #[serde(default)]
    flush_interval_ms: Option<u64>,
    #[allow(dead_code)]
    #[serde(default)]
    max_retries: Option<u32>,
    #[allow(dead_code)]
    #[serde(default)]
    enable_compression: Option<bool>,
}

#[derive(Deserialize)]
struct CaptureRequest {
    distinct_id: String,
    event: String,
    #[serde(default)]
    properties: Option<serde_json::Value>,
    #[serde(default)]
    timestamp: Option<String>,
}

#[derive(Serialize)]
struct HealthResponse {
    sdk_name: &'static str,
    sdk_version: &'static str,
    adapter_version: &'static str,
    capabilities: Vec<&'static str>,
}

#[derive(Serialize)]
struct StateResponse {
    pending_events: i64,
    total_events_captured: u64,
    total_events_sent: u64,
    total_retries: u64,
    last_error: Option<String>,
    requests_made: Vec<RequestRecord>,
}

fn parse_capture_mode() -> CaptureMode {
    match std::env::var("CAPTURE_MODE").as_deref() {
        Ok("v1") => CaptureMode::V1,
        _ => CaptureMode::V0,
    }
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let capability = match state.capture_mode {
        CaptureMode::V0 => "capture_v0",
        CaptureMode::V1 => "capture_v1",
    };
    Json(HealthResponse {
        sdk_name: "posthog-rs",
        sdk_version: env!("CARGO_PKG_VERSION"),
        adapter_version: "0.1.0",
        capabilities: vec![capability],
    })
}

async fn init(
    State(state): State<AppState>,
    Json(req): Json<InitRequest>,
) -> impl IntoResponse {
    let mut s = state.inner.lock().await;

    // Reset state
    *s = AdapterState::default();

    let options = ClientOptionsBuilder::default()
        .api_key(req.api_key)
        .host(req.host)
        .capture_mode(state.capture_mode)
        .build();

    match options {
        Ok(opts) => {
            let client = posthog_rs::client(opts).await;
            s.client = Some(client);
            Json(serde_json::json!({ "success": true })).into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            s.last_error = Some(msg.clone());
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": msg })),
            )
                .into_response()
        }
    }
}

async fn capture_event(
    State(state): State<AppState>,
    Json(req): Json<CaptureRequest>,
) -> impl IntoResponse {
    let mut s = state.inner.lock().await;

    let client = match &s.client {
        Some(c) => c,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "SDK not initialized" })),
            )
                .into_response();
        }
    };

    let mut event = Event::new(req.event, req.distinct_id);

    if let Some(props) = req.properties {
        if let Some(obj) = props.as_object() {
            for (k, v) in obj {
                let _ = event.insert_prop(k.clone(), v.clone());
            }
        }
    }

    if let Some(ts_str) = req.timestamp {
        if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&ts_str) {
            let _ = event.set_timestamp(ts);
        }
    }

    let uuid = uuid::Uuid::now_v7().to_string();

    match client.capture(event).await {
        Ok(()) => {
            s.total_events_captured += 1;
            s.pending_events += 1;
            // For V0, events are sent immediately by capture()
            s.total_events_sent += 1;
            s.pending_events -= 1;
            if s.pending_events < 0 {
                s.pending_events = 0;
            }
            Json(serde_json::json!({ "success": true, "uuid": uuid })).into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            s.total_events_captured += 1;
            s.last_error = Some(msg.clone());
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": msg })),
            )
                .into_response()
        }
    }
}

async fn flush(State(state): State<AppState>) -> impl IntoResponse {
    let s = state.inner.lock().await;

    if s.client.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "SDK not initialized" })),
        )
            .into_response();
    }

    // posthog-rs sends events immediately in capture(), so flush is a no-op
    Json(serde_json::json!({
        "success": true,
        "events_flushed": s.total_events_sent
    }))
    .into_response()
}

async fn get_state(State(state): State<AppState>) -> Json<StateResponse> {
    let s = state.inner.lock().await;
    Json(StateResponse {
        pending_events: s.pending_events,
        total_events_captured: s.total_events_captured,
        total_events_sent: s.total_events_sent,
        total_retries: s.total_retries,
        last_error: s.last_error.clone(),
        requests_made: s.requests_made.clone(),
    })
}

async fn reset(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mut s = state.inner.lock().await;
    *s = AdapterState::default();
    Json(serde_json::json!({ "success": true }))
}

#[tokio::main]
async fn main() {
    let capture_mode = parse_capture_mode();
    let mode_str = match capture_mode {
        CaptureMode::V0 => "v0",
        CaptureMode::V1 => "v1",
    };
    eprintln!("Starting posthog-rs SDK adapter (CAPTURE_MODE={mode_str})");

    let state = AppState {
        inner: Arc::new(Mutex::new(AdapterState::default())),
        capture_mode,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/init", post(init))
        .route("/capture", post(capture_event))
        .route("/flush", post(flush))
        .route("/state", get(get_state))
        .route("/reset", post(reset))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("failed to bind to port 8080");
    eprintln!("Listening on 0.0.0.0:8080");
    axum::serve(listener, app).await.expect("server error");
}

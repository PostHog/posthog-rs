use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use posthog_rs::{CaptureMode, Client, ClientOptionsBuilder, Event};

const SUPPORTS_PARALLEL: bool = true;

#[derive(Clone)]
struct AppState {
    instances: Arc<Mutex<HashMap<String, AdapterState>>>,
    capture_mode: CaptureMode,
}

const DEFAULT_TEST_ID: &str = "_global";

#[derive(Default)]
struct AdapterState {
    client: Option<Arc<Client>>,
    historical_migration: bool,
    total_events_captured: u64,
    total_events_sent: u64,
    last_error: Option<String>,
    pending_events: i64,
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
    #[serde(default)]
    disable_geoip: Option<bool>,
    #[serde(default)]
    historical_migration: Option<bool>,
}

#[derive(Deserialize)]
struct CaptureRequest {
    distinct_id: String,
    event: String,
    #[serde(default)]
    properties: Option<serde_json::Value>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    options: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct TestIdParam {
    #[serde(default)]
    test_id: Option<String>,
}

impl TestIdParam {
    fn key(&self) -> &str {
        self.test_id.as_deref().unwrap_or(DEFAULT_TEST_ID)
    }
}

#[derive(Serialize)]
struct HealthResponse {
    sdk_name: &'static str,
    sdk_version: &'static str,
    adapter_version: &'static str,
    capabilities: Vec<&'static str>,
    supports_parallel: bool,
}

#[derive(Serialize)]
struct StateResponse {
    pending_events: i64,
    total_events_captured: u64,
    total_events_sent: u64,
    total_retries: u64,
    last_error: Option<String>,
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
        adapter_version: env!("CARGO_PKG_VERSION"),
        capabilities: vec![capability],
        supports_parallel: SUPPORTS_PARALLEL,
    })
}

async fn init(
    State(state): State<AppState>,
    Query(params): Query<TestIdParam>,
    Json(req): Json<InitRequest>,
) -> impl IntoResponse {
    let mut instances = state.instances.lock().await;
    let s = instances.entry(params.key().to_string()).or_default();

    *s = AdapterState::default();
    s.historical_migration = req.historical_migration.unwrap_or(false);

    let mut builder = ClientOptionsBuilder::default();
    builder
        .api_key(req.api_key)
        .host(req.host)
        .capture_mode(state.capture_mode);

    if req.disable_geoip.unwrap_or(false) {
        builder.disable_geoip(true);
    }

    // Inject X-Test-Id header so the mock capture server can partition
    // requests by test when running in parallel mode.
    if let Some(ref test_id) = params.test_id {
        let mut headers = HashMap::new();
        headers.insert("X-Test-Id".to_string(), test_id.clone());
        builder.extra_capture_headers(headers);
    }

    match builder.build() {
        Ok(opts) => {
            let client = posthog_rs::client(opts).await;
            s.client = Some(Arc::new(client));
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
    Query(params): Query<TestIdParam>,
    Json(req): Json<CaptureRequest>,
) -> impl IntoResponse {
    let key = params.key().to_string();

    // Phase 1: short lock — clone Arc<Client>, bump in-flight counter
    let client = {
        let mut instances = state.instances.lock().await;
        let s = instances.entry(key.clone()).or_default();
        match &s.client {
            Some(c) => {
                s.total_events_captured += 1;
                s.pending_events += 1;
                c.clone()
            }
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "SDK not initialized" })),
                )
                    .into_response();
            }
        }
    };

    // Build event outside the lock
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

    // EventOptions wiring (set_options) is applied in the V1 capture
    // implementation branch where EventOptions exists on Event.
    let _ = req.options;

    let uuid = uuid::Uuid::now_v7().to_string();

    // Phase 2: network call without holding the lock
    let result = client.capture(event).await;

    // Phase 3: short lock — record outcome
    {
        let mut instances = state.instances.lock().await;
        if let Some(s) = instances.get_mut(&key) {
            s.pending_events = (s.pending_events - 1).max(0);
            match &result {
                Ok(()) => {
                    s.total_events_sent += 1;
                }
                Err(e) => {
                    s.last_error = Some(e.to_string());
                }
            }
        }
    }

    match result {
        Ok(()) => Json(serde_json::json!({ "success": true, "uuid": uuid })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn flush(
    State(state): State<AppState>,
    Query(params): Query<TestIdParam>,
) -> impl IntoResponse {
    let instances = state.instances.lock().await;
    let s = match instances.get(params.key()) {
        Some(s) => s,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "SDK not initialized" })),
            )
                .into_response();
        }
    };

    if s.client.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "SDK not initialized" })),
        )
            .into_response();
    }

    Json(serde_json::json!({
        "success": true,
        "events_flushed": s.total_events_sent
    }))
    .into_response()
}

async fn get_state(
    State(state): State<AppState>,
    Query(params): Query<TestIdParam>,
) -> impl IntoResponse {
    let instances = state.instances.lock().await;
    let s = instances.get(params.key());

    match s {
        Some(s) => Json(serde_json::json!(StateResponse {
            pending_events: s.pending_events,
            total_events_captured: s.total_events_captured,
            total_events_sent: s.total_events_sent,
            total_retries: 0,
            last_error: s.last_error.clone(),
        }))
        .into_response(),
        None => Json(serde_json::json!(StateResponse {
            pending_events: 0,
            total_events_captured: 0,
            total_events_sent: 0,
            total_retries: 0,
            last_error: None,
        }))
        .into_response(),
    }
}

async fn reset(
    State(state): State<AppState>,
    Query(params): Query<TestIdParam>,
) -> Json<serde_json::Value> {
    let mut instances = state.instances.lock().await;
    instances.remove(params.key());
    Json(serde_json::json!({ "success": true }))
}

#[tokio::main]
async fn main() {
    let capture_mode = parse_capture_mode();
    let mode_str = match capture_mode {
        CaptureMode::V0 => "v0",
        CaptureMode::V1 => "v1",
    };
    eprintln!(
        "Starting posthog-rs SDK adapter (CAPTURE_MODE={mode_str}, parallel={SUPPORTS_PARALLEL})"
    );

    let state = AppState {
        instances: Arc::new(Mutex::new(HashMap::new())),
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

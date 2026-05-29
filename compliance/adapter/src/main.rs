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

use posthog_rs::{CaptureCompression, CaptureMode, Client, ClientOptionsBuilder, Event};

const SUPPORTS_PARALLEL: bool = true;

#[derive(Clone)]
struct AppState {
    instances: Arc<Mutex<HashMap<String, AdapterState>>>,
    capture_mode: CaptureMode,
    compression: CaptureCompression,
}

const DEFAULT_TEST_ID: &str = "_global";

struct AdapterState {
    client: Option<Client>,
    historical_migration: bool,
    buffer: Vec<Event>,
    flush_at: Option<u32>,
    total_events_captured: u64,
    total_events_sent: u64,
    last_error: Option<String>,
    pending_events: i64,
}

impl Default for AdapterState {
    fn default() -> Self {
        Self {
            client: None,
            historical_migration: false,
            buffer: Vec::new(),
            flush_at: None,
            total_events_captured: 0,
            total_events_sent: 0,
            last_error: None,
            pending_events: 0,
        }
    }
}

#[derive(Deserialize)]
struct InitRequest {
    api_key: String,
    host: String,
    #[serde(default)]
    flush_at: Option<u32>,
    #[allow(dead_code)]
    #[serde(default)]
    flush_interval_ms: Option<u64>,
    #[serde(default)]
    max_retries: Option<u32>,
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
    capabilities: Vec<String>,
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

fn parse_compression() -> CaptureCompression {
    match std::env::var("COMPRESSION").as_deref() {
        Ok("deflate") => CaptureCompression::Deflate,
        Ok("br") => CaptureCompression::Br,
        Ok("zstd") => CaptureCompression::Zstd,
        _ => CaptureCompression::Gzip,
    }
}

fn compression_capability(c: CaptureCompression) -> &'static str {
    match c {
        CaptureCompression::Gzip => "encoding_gzip",
        CaptureCompression::Deflate => "encoding_deflate",
        CaptureCompression::Br => "encoding_br",
        CaptureCompression::Zstd => "encoding_zstd",
    }
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let mut capabilities: Vec<String> = Vec::new();
    match state.capture_mode {
        CaptureMode::V0 => capabilities.push("capture_v0".to_string()),
        CaptureMode::V1 => {
            capabilities.push("capture_v1".to_string());
            // Only the configured algorithm is advertised: a single adapter
            // instance compresses one way, so advertising the others would run
            // compression tests it cannot satisfy. CI varies COMPRESSION to
            // cover the full matrix.
            capabilities.push(compression_capability(state.compression).to_string());
        }
    }
    Json(HealthResponse {
        sdk_name: "posthog-rs",
        sdk_version: env!("CARGO_PKG_VERSION"),
        adapter_version: "0.3.0",
        capabilities,
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
    s.flush_at = req.flush_at;

    let mut builder = ClientOptionsBuilder::default();
    builder
        .api_key(req.api_key)
        .host(req.host)
        .capture_mode(state.capture_mode);

    // The harness `max_retries` counts retries; the SDK option counts total
    // attempts (initial + retries), so add one.
    if let Some(retries) = req.max_retries {
        builder.max_capture_retries(retries.saturating_add(1));
    }

    if req.enable_compression.unwrap_or(false) {
        builder.capture_compression(state.compression);
    }

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

/// Send all buffered events as a single batch, leaving the buffer empty.
async fn flush_buffer(s: &mut AdapterState) {
    if s.buffer.is_empty() {
        return;
    }
    let client = match &s.client {
        Some(c) => c,
        None => return,
    };
    let events = std::mem::take(&mut s.buffer);
    let count = events.len() as u64;
    match client.capture_batch(events, s.historical_migration).await {
        Ok(()) => {
            s.total_events_sent += count;
            s.pending_events = (s.pending_events - count as i64).max(0);
        }
        Err(e) => {
            s.last_error = Some(e.to_string());
            s.pending_events = (s.pending_events - count as i64).max(0);
        }
    }
}

async fn capture_event(
    State(state): State<AppState>,
    Query(params): Query<TestIdParam>,
    Json(req): Json<CaptureRequest>,
) -> impl IntoResponse {
    let mut instances = state.instances.lock().await;
    let s = instances.entry(params.key().to_string()).or_default();

    if s.client.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "SDK not initialized" })),
        )
            .into_response();
    }

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

    if let Some(opts_val) = req.options {
        if let Some(obj) = opts_val.as_object() {
            event.set_options(|opts| {
                if let Some(v) = obj.get("cookieless_mode").and_then(|v| v.as_bool()) {
                    opts.cookieless_mode = Some(v);
                }
                if let Some(v) = obj.get("disable_skew_correction").and_then(|v| v.as_bool()) {
                    opts.disable_skew_correction = Some(v);
                }
                if let Some(v) = obj.get("process_person_profile").and_then(|v| v.as_bool()) {
                    opts.process_person_profile = Some(v);
                }
                if let Some(v) = obj.get("product_tour_id").and_then(|v| v.as_str()) {
                    opts.product_tour_id = Some(v.to_string());
                }
            });
        }
    }

    let uuid = uuid::Uuid::now_v7().to_string();

    s.total_events_captured += 1;
    s.pending_events += 1;
    s.buffer.push(event);

    // Auto-flush once the buffer reaches the configured threshold.
    if let Some(threshold) = s.flush_at {
        if s.buffer.len() as u32 >= threshold {
            flush_buffer(s).await;
        }
    }

    Json(serde_json::json!({ "success": true, "uuid": uuid })).into_response()
}

async fn flush(
    State(state): State<AppState>,
    Query(params): Query<TestIdParam>,
) -> impl IntoResponse {
    let mut instances = state.instances.lock().await;
    let s = match instances.get_mut(params.key()) {
        Some(s) if s.client.is_some() => s,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "SDK not initialized" })),
            )
                .into_response();
        }
    };

    flush_buffer(s).await;

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
    match instances.get(params.key()) {
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
    let compression = parse_compression();
    let mode_str = match capture_mode {
        CaptureMode::V0 => "v0",
        CaptureMode::V1 => "v1",
    };
    eprintln!(
        "Starting posthog-rs SDK adapter (CAPTURE_MODE={mode_str}, compression={}, parallel={SUPPORTS_PARALLEL})",
        compression_capability(compression)
    );

    let state = AppState {
        instances: Arc::new(Mutex::new(HashMap::new())),
        capture_mode,
        compression,
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

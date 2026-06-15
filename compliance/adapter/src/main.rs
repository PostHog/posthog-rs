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

use posthog_rs::{CaptureCompression, Client, ClientOptionsBuilder, Event};

const SUPPORTS_PARALLEL: bool = true;

#[derive(Clone)]
struct AppState {
    instances: Arc<Mutex<HashMap<String, AdapterState>>>,
    compression: Option<CaptureCompression>,
}

const DEFAULT_TEST_ID: &str = "_global";

// The SDK now owns batching, retry, and the queue; the adapter just forwards
// capture/flush/shutdown and tracks how many captures it handed off.
#[derive(Default)]
struct AdapterState {
    client: Option<Arc<Client>>,
    historical_migration: bool,
    total_events_captured: u64,
    last_error: Option<String>,
}

#[derive(Deserialize)]
struct InitRequest {
    api_key: String,
    host: String,
    #[serde(default)]
    flush_at: Option<u32>,
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
    #[cfg_attr(not(feature = "capture-v1"), allow(dead_code))]
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
    // Always 0: capture API does not expose retry counts; retries are validated at the wire level.
    total_retries: u64,
    last_error: Option<String>,
}

fn parse_compression() -> Option<CaptureCompression> {
    match std::env::var("COMPRESSION").as_deref() {
        Ok("gzip") => Some(CaptureCompression::Gzip),
        Ok("deflate") => Some(CaptureCompression::Deflate),
        Ok("br") => Some(CaptureCompression::Br),
        Ok("zstd") => Some(CaptureCompression::Zstd),
        _ => None,
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
    if cfg!(feature = "capture-v1") {
        capabilities.push("capture_v1".to_string());
    } else {
        capabilities.push("capture_v0".to_string());
    }
    if let Some(algo) = state.compression {
        capabilities.push(compression_capability(algo).to_string());
    }
    Json(HealthResponse {
        // Per-build name so the v0 and v1 compliance jobs post distinct PR
        // comments instead of overwriting one shared report.
        sdk_name: if cfg!(feature = "capture-v1") {
            "posthog-rs-v1"
        } else {
            "posthog-rs-v0"
        },
        sdk_version: env!("CARGO_PKG_VERSION"),
        adapter_version: env!("CARGO_PKG_VERSION"),
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

    let mut builder = ClientOptionsBuilder::default();
    builder.api_key(req.api_key).host(req.host);

    // Batching knobs are now owned by the SDK.
    if let Some(flush_at) = req.flush_at {
        builder.flush_at(flush_at as usize);
    }
    if let Some(interval_ms) = req.flush_interval_ms {
        builder.flush_interval_ms(interval_ms);
    }

    // The harness `max_retries` counts retries; the SDK option counts total
    // attempts (initial + retries), so add one.
    if let Some(retries) = req.max_retries {
        builder.max_capture_attempts(retries.saturating_add(1));
    }

    if req.enable_compression.unwrap_or(false) {
        if let Some(algo) = state.compression {
            builder.capture_compression(algo);
        }
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
    #[cfg(feature = "capture-v1")]
    if let Some(opts_val) = req.options {
        if let Some(obj) = opts_val.as_object() {
            for (k, v) in obj {
                let _ = event.set_option(k, v.clone());
            }
        }
    }

    // Snapshot the client out of the lock so the (non-blocking) enqueue and any
    // awaits don't hold the instances mutex.
    let (client, historical_migration) = {
        let mut instances = state.instances.lock().await;
        let s = instances.entry(key).or_default();
        let Some(client) = s.client.clone() else {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "SDK not initialized" })),
            )
                .into_response();
        };
        s.total_events_captured += 1;
        (client, s.historical_migration)
    };

    // capture_batch carries the historical_migration flag through to the worker;
    // a single-event vec is just a non-blocking enqueue.
    let _ = client
        .capture_batch(vec![event], historical_migration)
        .await;

    let uuid = uuid::Uuid::now_v7().to_string();
    Json(serde_json::json!({ "success": true, "uuid": uuid })).into_response()
}

async fn flush(
    State(state): State<AppState>,
    Query(params): Query<TestIdParam>,
) -> impl IntoResponse {
    let client = {
        let instances = state.instances.lock().await;
        match instances.get(params.key()).and_then(|s| s.client.clone()) {
            Some(c) => c,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "SDK not initialized" })),
                )
                    .into_response();
            }
        }
    };

    let before = client.pending_events() as u64;
    client.flush().await;
    let flushed = before.saturating_sub(client.pending_events() as u64);

    Json(serde_json::json!({
        "success": true,
        "events_flushed": flushed
    }))
    .into_response()
}

async fn shutdown(
    State(state): State<AppState>,
    Query(params): Query<TestIdParam>,
) -> impl IntoResponse {
    let client = {
        let instances = state.instances.lock().await;
        match instances.get(params.key()).and_then(|s| s.client.clone()) {
            Some(c) => c,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "SDK not initialized" })),
                )
                    .into_response();
            }
        }
    };

    client.shutdown().await;
    Json(serde_json::json!({ "success": true })).into_response()
}

async fn get_state(
    State(state): State<AppState>,
    Query(params): Query<TestIdParam>,
) -> impl IntoResponse {
    let instances = state.instances.lock().await;
    match instances.get(params.key()) {
        Some(s) => {
            // `pending_events` reflects everything the SDK still holds in flight
            // (channel + worker buffer + retries), so "sent" = captured - pending.
            // Caveat: capture() is fire-and-forget, so an event dropped because the
            // SDK queue was full still counts toward `total_events_captured` and is
            // therefore counted as sent here. An exact sent-count would need an SDK
            // delivery callback; the acceptance suite never fills the queue, so this
            // approximation is accurate for it.
            let pending = s.client.as_ref().map_or(0, |c| c.pending_events()) as i64;
            Json(serde_json::json!(StateResponse {
                pending_events: pending,
                total_events_captured: s.total_events_captured,
                total_events_sent: (s.total_events_captured as i64 - pending).max(0) as u64,
                total_retries: 0,
                last_error: s.last_error.clone(),
            }))
            .into_response()
        }
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
    let compression = parse_compression();
    let mode_str = if cfg!(feature = "capture-v1") {
        "v1"
    } else {
        "v0"
    };
    let compression_str = match compression {
        Some(algo) => compression_capability(algo),
        None => "none",
    };
    eprintln!(
        "Starting posthog-rs SDK adapter (capture={mode_str}, compression={compression_str}, parallel={SUPPORTS_PARALLEL})"
    );

    let state = AppState {
        instances: Arc::new(Mutex::new(HashMap::new())),
        compression,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/init", post(init))
        .route("/capture", post(capture_event))
        .route("/flush", post(flush))
        .route("/shutdown", post(shutdown))
        .route("/state", get(get_state))
        .route("/reset", post(reset))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("failed to bind to port 8080");
    eprintln!("Listening on 0.0.0.0:8080");
    axum::serve(listener, app).await.expect("server error");
}

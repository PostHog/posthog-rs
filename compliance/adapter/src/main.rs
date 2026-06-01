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

#[derive(Default)]
struct AdapterState {
    client: Option<Arc<Client>>,
    historical_migration: bool,
    buffer: Vec<Event>,
    flush_at: Option<u32>,
    total_events_captured: u64,
    total_events_sent: u64,
    last_error: Option<String>,
    pending_events: i64,
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
        if let Some(algo) = state.compression {
            capabilities.push(compression_capability(algo).to_string());
        }
    } else {
        capabilities.push("capture_v0".to_string());
    }
    Json(HealthResponse {
        sdk_name: "posthog-rs",
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
    s.flush_at = req.flush_at;

    let mut builder = ClientOptionsBuilder::default();
    builder.api_key(req.api_key).host(req.host);

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

/// Drain pending events from the buffer under the lock, then send the batch
/// without holding the lock, and finally re-acquire the lock to record the
/// outcome. Returns the total number of events sent.
async fn flush_buffer(instances: &Mutex<HashMap<String, AdapterState>>, key: &str) -> u64 {
    let (client, events, historical_migration) = {
        let mut map = instances.lock().await;
        let s = match map.get_mut(key) {
            Some(s) => s,
            None => return 0,
        };
        if s.buffer.is_empty() {
            return 0;
        }
        let client = match &s.client {
            Some(c) => c.clone(),
            None => return 0,
        };
        let events = std::mem::take(&mut s.buffer);
        (client, events, s.historical_migration)
    };

    let count = events.len() as u64;
    let result = client.capture_batch(events, historical_migration).await;

    let total_sent = {
        let mut map = instances.lock().await;
        if let Some(s) = map.get_mut(key) {
            s.pending_events = (s.pending_events - count as i64).max(0);
            match result {
                Ok(()) => s.total_events_sent += count,
                Err(e) => s.last_error = Some(e.to_string()),
            }
            s.total_events_sent
        } else {
            0
        }
    };

    total_sent
}

async fn capture_event(
    State(state): State<AppState>,
    Query(params): Query<TestIdParam>,
    Json(req): Json<CaptureRequest>,
) -> impl IntoResponse {
    let key = params.key().to_string();

    let needs_flush = {
        let mut instances = state.instances.lock().await;
        let s = instances.entry(key.clone()).or_default();

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

        #[cfg(feature = "capture-v1")]
        if let Some(opts_val) = req.options {
            if let Some(obj) = opts_val.as_object() {
                for (k, v) in obj {
                    let _ = event.set_option(k, v.clone());
                }
            }
        }

        s.total_events_captured += 1;
        s.pending_events += 1;
        s.buffer.push(event);

        matches!(s.flush_at, Some(threshold) if s.buffer.len() as u32 >= threshold)
    };

    if needs_flush {
        flush_buffer(&state.instances, &key).await;
    }

    let uuid = uuid::Uuid::now_v7().to_string();
    Json(serde_json::json!({ "success": true, "uuid": uuid })).into_response()
}

async fn flush(
    State(state): State<AppState>,
    Query(params): Query<TestIdParam>,
) -> impl IntoResponse {
    let key = params.key().to_string();

    {
        let instances = state.instances.lock().await;
        match instances.get(&key) {
            Some(s) if s.client.is_some() => {}
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "SDK not initialized" })),
                )
                    .into_response();
            }
        }
    }

    let total_sent = flush_buffer(&state.instances, &key).await;

    Json(serde_json::json!({
        "success": true,
        "events_flushed": total_sent
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
        .route("/state", get(get_state))
        .route("/reset", post(reset))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("failed to bind to port 8080");
    eprintln!("Listening on 0.0.0.0:8080");
    axum::serve(listener, app).await.expect("server error");
}

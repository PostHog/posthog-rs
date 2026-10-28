use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

use posthog_rs::{
    CaptureCompression, Client, ClientOptionsBuilder, EvaluateFlagsOptions, Event, EventStatus,
    PostHogError,
};

const SUPPORTS_PARALLEL: bool = true;

#[derive(Clone)]
struct AppState {
    instances: Arc<Mutex<HashMap<String, AdapterState>>>,
    compression: Option<CaptureCompression>,
}

const DEFAULT_TEST_ID: &str = "_global";

const PADDING_KEY: &str = "$test_padding";

// The SDK now owns batching, retry, and the queue; the adapter just forwards
// capture/flush/shutdown and tracks how many captures it handed off.
#[derive(Default)]
struct AdapterState {
    client: Option<Arc<Client>>,
    historical_migration: bool,
    total_events_captured: u64,
    last_error: Option<String>,
    ledger: Arc<DropLedger>,
    /// Length of the shared SDK log when `/init` ran; `/state` returns lines after it.
    log_start: usize,
}

#[derive(Clone, Serialize)]
struct ReportedDrop {
    uuid: String,
    event: Option<String>,
    details: Option<String>,
}

/// The events the adapter captured and the drops the SDK's `on_error` hook reported for them.
#[derive(Default)]
struct DropLedger {
    /// Event names per uuid in capture order; the first copy is the one capture keeps.
    names: StdMutex<HashMap<uuid::Uuid, Vec<String>>>,
    drops: StdMutex<Vec<ReportedDrop>>,
}

impl DropLedger {
    fn record_capture(&self, uuid: uuid::Uuid, event: &str) {
        self.names
            .lock()
            .unwrap()
            .entry(uuid)
            .or_default()
            .push(event.to_string());
    }

    /// Record every event the failure lost: each dropped or unpersisted verdict, plus the
    /// later copies capture dropped as duplicate uuids.
    fn record_failure(&self, failure: &PostHogError<'_>) {
        let PostHogError::Capture(capture) = failure else {
            return;
        };
        let names = self.names.lock().unwrap();
        let mut drops = self.drops.lock().unwrap();
        for (uuid, result) in capture.event_results() {
            let copies = names.get(uuid);
            if matches!(result.result, EventStatus::Drop | EventStatus::Retry) {
                drops.push(ReportedDrop {
                    uuid: uuid.to_string(),
                    event: copies.and_then(|c| c.first()).cloned(),
                    details: result.details.clone(),
                });
            }
            for copy in 1..=result.duplicates_dropped.unwrap_or(0) as usize {
                drops.push(ReportedDrop {
                    uuid: uuid.to_string(),
                    event: copies.and_then(|c| c.get(copy)).cloned(),
                    details: Some("duplicate_event_uuid".to_string()),
                });
            }
        }
    }

    fn drops(&self) -> Vec<ReportedDrop> {
        self.drops.lock().unwrap().clone()
    }
}

/// Every line the SDK logged since the adapter started. Log events carry no test id, so
/// with parallel tests a line can show up in another test's `/state`.
fn sdk_log() -> &'static StdMutex<Vec<String>> {
    static LOG: OnceLock<StdMutex<Vec<String>>> = OnceLock::new();
    LOG.get_or_init(Default::default)
}

/// Records events from the posthog crate as `LEVEL target: message key=value ...`.
struct SdkLogLayer;

struct LineVisitor {
    message: String,
    fields: String,
}

impl Visit for LineVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for SdkLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        if !meta.target().starts_with("posthog_rs") || *meta.level() > tracing::Level::DEBUG {
            return;
        }
        let mut visitor = LineVisitor {
            message: String::new(),
            fields: String::new(),
        };
        event.record(&mut visitor);
        let line = format!(
            "{} {}: {}{}",
            meta.level(),
            meta.target(),
            visitor.message,
            visitor.fields
        );
        sdk_log().lock().unwrap().push(line);
    }
}

fn install_sdk_log_capture() {
    tracing_subscriber::registry().with(SdkLogLayer).init();
}

/// Pad `properties` with one string property so their compact JSON is exactly `size` bytes.
fn padded_properties(
    properties: Option<serde_json::Value>,
    size: usize,
    marker: &str,
) -> Result<serde_json::Value, String> {
    let mut map = match properties {
        None | Some(serde_json::Value::Null) => serde_json::Map::new(),
        Some(serde_json::Value::Object(map)) => map,
        Some(_) => return Err("properties must be an object to pad".to_string()),
    };
    map.insert(PADDING_KEY.to_string(), marker.into());
    let with_marker = serde_json::to_vec(&map).map_err(|e| e.to_string())?.len();
    let Some(fill) = size.checked_sub(with_marker) else {
        return Err(format!(
            "pad_properties_to_bytes {size} is smaller than the properties and marker ({with_marker} bytes)"
        ));
    };
    map.insert(
        PADDING_KEY.to_string(),
        format!("{marker}{}", "x".repeat(fill)).into(),
    );
    Ok(serde_json::Value::Object(map))
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
    #[serde(default)]
    super_properties: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    before_send: Option<BeforeSendRequest>,
}

#[derive(Deserialize)]
struct BeforeSendRequest {
    #[serde(default)]
    set_properties: serde_json::Map<String, serde_json::Value>,
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
    options: Option<serde_json::Map<String, serde_json::Value>>,
    /// Sent only on `/capture_ai`; the event keeps it on the wire.
    #[serde(default)]
    uuid: Option<uuid::Uuid>,
    /// Pad `properties` to exactly this many compact-JSON bytes (`generated_payloads`).
    #[serde(default)]
    pad_properties_to_bytes: Option<usize>,
    /// Start the padding string with this text.
    #[serde(default)]
    marker: Option<String>,
}

impl CaptureRequest {
    /// Build the event as an SDK user would, with properties and options
    /// unchanged, and return its UUID.
    fn into_event(self) -> Result<(Event, uuid::Uuid), String> {
        let properties = match self.pad_properties_to_bytes {
            Some(size) => Some(padded_properties(
                self.properties,
                size,
                self.marker.as_deref().unwrap_or_default(),
            )?),
            None => self.properties,
        };
        let mut event = Event::new(self.event, self.distinct_id);
        if let Some(props) = properties {
            if let Some(obj) = props.as_object() {
                for (k, v) in obj {
                    let _ = event.insert_prop(k.clone(), v.clone());
                }
            }
        }
        if let Some(ts_str) = self.timestamp {
            if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&ts_str) {
                let _ = event.set_timestamp(ts);
            }
        }
        for (key, value) in self.options.unwrap_or_default() {
            event
                .insert_option(key, value)
                .expect("a JSON value always serializes");
        }
        let uuid = self.uuid.unwrap_or_else(uuid::Uuid::now_v7);
        event.set_uuid(uuid);
        Ok((event, uuid))
    }
}

#[derive(Deserialize)]
struct FeatureFlagRequest {
    key: String,
    distinct_id: String,
    #[serde(default)]
    person_properties: Option<HashMap<String, serde_json::Value>>,
    #[serde(default)]
    groups: Option<HashMap<String, String>>,
    #[serde(default)]
    group_properties: Option<HashMap<String, HashMap<String, serde_json::Value>>>,
    #[serde(default)]
    disable_geoip: Option<bool>,
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
    reported_drops: Vec<ReportedDrop>,
    log_lines: Vec<String>,
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

async fn build_client(options: posthog_rs::ClientOptions) -> Client {
    #[cfg(feature = "async-client")]
    {
        posthog_rs::client(options).await
    }
    #[cfg(not(feature = "async-client"))]
    {
        posthog_rs::client(options)
    }
}

async fn evaluate_flags(
    client: &Client,
    distinct_id: String,
    options: EvaluateFlagsOptions,
) -> Result<posthog_rs::FeatureFlagEvaluations, posthog_rs::Error> {
    #[cfg(feature = "async-client")]
    {
        client.evaluate_flags(distinct_id, options).await
    }
    #[cfg(not(feature = "async-client"))]
    {
        client.evaluate_flags(distinct_id, options)
    }
}

async fn flush_client(client: &Client) {
    #[cfg(feature = "async-client")]
    client.flush().await;
    #[cfg(not(feature = "async-client"))]
    client.flush();
}

async fn shutdown_client(client: &Client) {
    #[cfg(feature = "async-client")]
    client.shutdown().await;
    #[cfg(not(feature = "async-client"))]
    client.shutdown();
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    // Frozen harness contract keys that select suites and tests via `requires:`.
    let mut capabilities: Vec<String> = [
        "capture_v1",
        "capture_ai_v1",
        "event_options",
        "event_uuid_override",
        "bounded_retries",
        "disable_geoip",
        "historical_migration",
        "reported_drops",
        "log_capture",
        "generated_payloads",
        "fail_open",
    ]
    .map(String::from)
    .to_vec();
    if let Some(algo) = state.compression {
        capabilities.push(compression_capability(algo).to_string());
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
    s.log_start = sdk_log().lock().unwrap().len();

    // posthog-rs has no super (default) properties, so `property_layers` stays undeclared.
    if req.super_properties.is_some_and(|props| !props.is_empty()) {
        let msg = "posthog-rs has no super properties".to_string();
        s.last_error = Some(msg.clone());
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": msg })),
        )
            .into_response();
    }

    // An empty or personal key must reach the SDK, which disables itself (`fail_open`).
    let mut builder = ClientOptionsBuilder::default();
    builder.api_key(req.api_key).host(req.host);

    let ledger = s.ledger.clone();
    builder.on_error(move |failure| ledger.record_failure(failure));

    if let Some(before_send) = req.before_send {
        let set_properties = before_send.set_properties;
        builder.before_send(move |mut event| {
            for (key, value) in &set_properties {
                let _ = event.insert_prop(key.clone(), value.clone());
            }
            Some(event)
        });
    }

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
            builder
                .capture_compression(algo)
                .capture_ai_compression(algo);
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
            let client = build_client(opts).await;
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

/// The SDK capture method a harness request goes through.
#[derive(Clone, Copy)]
enum Lane {
    Analytics,
    Ai,
}

async fn capture_event(
    State(state): State<AppState>,
    Query(params): Query<TestIdParam>,
    Json(req): Json<CaptureRequest>,
) -> impl IntoResponse {
    enqueue(state, params, req, Lane::Analytics).await
}

async fn capture_ai_event(
    State(state): State<AppState>,
    Query(params): Query<TestIdParam>,
    Json(req): Json<CaptureRequest>,
) -> impl IntoResponse {
    enqueue(state, params, req, Lane::Ai).await
}

async fn enqueue(
    state: AppState,
    params: TestIdParam,
    req: CaptureRequest,
    lane: Lane,
) -> axum::response::Response {
    let key = params.key().to_string();
    let (event, uuid) = match req.into_event() {
        Ok(built) => built,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": msg })),
            )
                .into_response();
        }
    };

    // Snapshot the client out of the lock so the (non-blocking) enqueue and any
    // awaits don't hold the instances mutex.
    let (client, historical_migration, ledger) = {
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
        (client, s.historical_migration, s.ledger.clone())
    };

    // Before the enqueue, so a drop reported for the uuid always finds its event name.
    ledger.record_capture(uuid, event.event_name());

    // The batch methods carry historical_migration; a one-event vec is still a non-blocking enqueue.
    match lane {
        Lane::Analytics => client.capture_batch(vec![event], historical_migration),
        Lane::Ai => client.capture_ai_batch(vec![event], historical_migration),
    }

    Json(serde_json::json!({ "success": true, "uuid": uuid.to_string() })).into_response()
}

async fn get_feature_flag(
    State(state): State<AppState>,
    Query(params): Query<TestIdParam>,
    Json(req): Json<FeatureFlagRequest>,
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

    let key = req.key;
    let mut options = EvaluateFlagsOptions::default();
    options.groups = req.groups;
    options.person_properties = req.person_properties;
    options.group_properties = req.group_properties;
    options.disable_geoip = req.disable_geoip;
    options.flag_keys = Some(vec![key.clone()]);

    match evaluate_flags(&client, req.distinct_id, options).await {
        Ok(flags) => {
            let value = flags.get_flag(&key);
            Json(serde_json::json!({ "success": true, "value": value })).into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            let mut instances = state.instances.lock().await;
            let s = instances.entry(params.key().to_string()).or_default();
            s.last_error = Some(msg.clone());
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": msg })),
            )
                .into_response()
        }
    }
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
    flush_client(&client).await;
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

    shutdown_client(&client).await;
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
                reported_drops: s.ledger.drops(),
                log_lines: sdk_log().lock().unwrap()[s.log_start..].to_vec(),
            }))
            .into_response()
        }
        None => Json(serde_json::json!(StateResponse {
            pending_events: 0,
            total_events_captured: 0,
            total_events_sent: 0,
            total_retries: 0,
            last_error: None,
            reported_drops: Vec::new(),
            log_lines: Vec::new(),
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
    if instances.is_empty() {
        sdk_log().lock().unwrap().clear();
    }
    Json(serde_json::json!({ "success": true }))
}

#[tokio::main]
async fn main() {
    install_sdk_log_capture();
    let compression = parse_compression();
    let compression_str = match compression {
        Some(algo) => compression_capability(algo),
        None => "none",
    };
    let client_mode = if cfg!(feature = "async-client") {
        "async"
    } else {
        "blocking"
    };
    eprintln!(
        "Starting posthog-rs SDK adapter (client={client_mode}, compression={compression_str}, parallel={SUPPORTS_PARALLEL})"
    );

    let state = AppState {
        instances: Arc::new(Mutex::new(HashMap::new())),
        compression,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/init", post(init))
        .route("/capture", post(capture_event))
        .route("/capture_ai", post(capture_ai_event))
        .route("/get_feature_flag", post(get_feature_flag))
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

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::CaptureRequest;

    fn request(body: Value) -> CaptureRequest {
        serde_json::from_value(body).expect("valid /capture body")
    }

    /// The built event's serde fields and the UUID `/capture` reports.
    fn event_json(req: CaptureRequest) -> (Value, uuid::Uuid) {
        let (event, uuid) = req.into_event().expect("valid event");
        (serde_json::to_value(&event).unwrap(), uuid)
    }

    #[test]
    fn options_and_properties_reach_the_sdk_unchanged() {
        // PostHog judges option values and the SDK owns the legacy fallback, so
        // nothing is translated here.
        let options = json!({
            "process_person_profile": "not-a-bool",
            "future_option": {"nested": [1, "two"]},
            "cookieless_mode": null,
        });
        let properties = json!({"$cookieless_mode": true, "plan": "pro"});
        let (event, _) = event_json(request(json!({
            "distinct_id": "user-1",
            "event": "signed_up",
            "properties": properties,
            "options": options,
        })));
        assert_eq!(event["options"], options);
        assert_eq!(event["properties"], properties);
    }

    #[test]
    fn returned_uuid_is_the_event_uuid() {
        let (event, uuid) = event_json(request(json!({"distinct_id": "u", "event": "e"})));
        assert_eq!(event["uuid"], uuid.to_string());
    }

    #[test]
    fn supplied_uuid_is_kept() {
        let supplied = "0198c0de-0000-7000-8000-000000000abc";
        let (event, uuid) = event_json(request(
            json!({"distinct_id": "u", "event": "e", "uuid": supplied}),
        ));
        assert_eq!(uuid.to_string(), supplied);
        assert_eq!(event["uuid"], supplied);
    }

    #[test]
    fn padding_makes_compact_properties_exactly_the_requested_size() {
        for (properties, marker) in [(None, ""), (Some(json!({"case": "é\"x"})), "MARK-é")] {
            let padded = super::padded_properties(properties, 5000, marker).unwrap();
            assert_eq!(serde_json::to_vec(&padded).unwrap().len(), 5000);
            assert!(padded["$test_padding"]
                .as_str()
                .unwrap()
                .starts_with(marker));
        }
    }

    #[test]
    fn padding_smaller_than_the_properties_is_rejected() {
        let result = super::padded_properties(Some(json!({"case": "x"})), 10, "");
        assert!(result.is_err());
    }

    #[test]
    fn non_object_options_are_rejected() {
        let body = json!({"distinct_id": "u", "event": "e", "options": ["cookieless_mode"]});
        assert!(serde_json::from_value::<CaptureRequest>(body).is_err());
    }
}

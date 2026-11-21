//! Shared fixtures for stateful client integration tests.
// Each test binary uses only some of these fixtures.
#![allow(dead_code)]

use std::io::Read;
use std::sync::{Arc, Mutex};

use httpmock::{HttpMockRequest, HttpMockResponse};

/// A capture v1 reply that accepts every event in the request, one result per
/// uuid, as capture does. The client counts an event that a 2xx reply does not
/// mention as delivered, so this reply only mirrors capture's shape.
pub fn accept_all(request: &HttpMockRequest) -> HttpMockResponse {
    let raw = request.body_vec();
    let mut body = Vec::new();
    if raw.starts_with(&[0x1f, 0x8b]) {
        flate2::read::GzDecoder::new(raw.as_slice())
            .read_to_end(&mut body)
            .unwrap();
    } else {
        body = raw;
    }
    let batch: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let results: serde_json::Map<String, serde_json::Value> = batch["batch"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .map(|event| {
                    (
                        event["uuid"].as_str().unwrap().to_owned(),
                        serde_json::json!({"result": "ok"}),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    HttpMockResponse::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(serde_json::json!({ "results": results }).to_string())
        .build()
}

#[derive(Clone)]
struct Recorder(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Recorder {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Keeps a second dispatch alive for the whole test process. With a single
/// dispatch, tracing-core registers a new callsite against the registering
/// thread's default, so a thread without a recorder could hide a log line from
/// a thread that has one.
fn keep_callsites_shared() {
    static REGISTRATION: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    REGISTRATION.get_or_init(|| tracing::Dispatch::new(tracing_subscriber::registry()));
}

/// Records this thread's log output at `level` and above while the guard lives.
pub fn record_logs(
    level: tracing::Level,
) -> (Arc<Mutex<Vec<u8>>>, tracing::subscriber::DefaultGuard) {
    keep_callsites_shared();
    let output = Arc::new(Mutex::new(Vec::new()));
    let sink = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(level)
        .with_writer(move || Recorder(sink.clone()))
        .finish();
    (output, tracing::subscriber::set_default(subscriber))
}

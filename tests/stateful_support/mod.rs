//! Shared fixtures for stateful client integration tests.

use std::io::Read;

use httpmock::{HttpMockRequest, HttpMockResponse};

/// A capture v1 reply that accepts every event in the request, one result per
/// uuid, as capture does. The stateful client retries a 2xx that does not
/// confirm each event, so capture mocks must answer this way.
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

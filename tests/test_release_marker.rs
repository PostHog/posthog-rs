// A binary that was never injected — every test binary — keeps the nil placeholder, so it must
// send no `$release_id`. The patched case is covered by the unit test in `src/release_marker.rs`.
#![cfg(feature = "error-tracking")]

use httpmock::prelude::*;
use std::error::Error as StdError;
use std::fmt;

#[derive(Debug)]
struct TestError;
impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "boom")
    }
}
impl StdError for TestError {}

#[cfg(not(feature = "capture-v1"))]
const CAPTURE_PATH: &str = "/batch/";
#[cfg(feature = "capture-v1")]
const CAPTURE_PATH: &str = "/i/v1/analytics/events";

/// The request carries no `$release_id`. A `when` matcher, so a request that does carry one
/// matches no mock and `assert_calls(1)` fails.
fn body_has_no_release_id(req: &HttpMockRequest) -> bool {
    std::str::from_utf8(req.body_ref())
        .map(|body| !body.contains("$release_id"))
        .unwrap_or(false)
}

#[cfg(not(feature = "async-client"))]
#[test]
fn an_uninjected_binary_sends_no_release_id() {
    assert_eq!(posthog_rs::injected_release_id(), None);

    let server = MockServer::start();
    let capture = server.mock(|when, then| {
        when.method(POST)
            .path(CAPTURE_PATH)
            .is_true(body_has_no_release_id);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({ "results": {} }));
    });

    let client = posthog_rs::client(("test_api_key", server.base_url().as_str()));
    client.capture_exception(&TestError).unwrap();
    client.flush();

    capture.assert_calls(1);
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn an_uninjected_binary_sends_no_release_id() {
    assert_eq!(posthog_rs::injected_release_id(), None);

    let server = MockServer::start();
    let capture = server.mock(|when, then| {
        when.method(POST)
            .path(CAPTURE_PATH)
            .is_true(body_has_no_release_id);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({ "results": {} }));
    });

    let client = posthog_rs::client(("test_api_key", server.base_url().as_str())).await;
    client.capture_exception(&TestError).await.unwrap();
    client.flush().await;

    capture.assert_calls(1);
}

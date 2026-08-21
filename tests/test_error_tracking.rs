// Error Tracking integration tests over the capture transport.
// The mock responds with an empty results map and the client runs a single
// attempt, so each test asserts exactly one well-formed request.
#![cfg(feature = "error-tracking")]

use httpmock::prelude::*;
use serde_json::json;
use std::error::Error as StdError;
use std::fmt;

#[derive(Debug)]
struct TestError;

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "payment failed")
    }
}

impl StdError for TestError {}

#[derive(Debug)]
struct PanicDisplayError;

impl fmt::Display for PanicDisplayError {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        panic!("disabled clients should not build exception payloads")
    }
}

impl StdError for PanicDisplayError {}

const CAPTURE_PATH: &str = "/i/v1/analytics/events";

fn ok_response() -> serde_json::Value {
    json!({ "results": {} })
}

fn last_exception_stack_function(body: &serde_json::Value) -> &str {
    body.pointer("/batch/0/properties/$exception_list/0/stacktrace/frames")
        .and_then(|value| value.as_array())
        .and_then(|frames| frames.last())
        .and_then(|frame| frame.get("function"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
}

fn request_has_capture_exception_user_frame_last(req: &HttpMockRequest) -> bool {
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(req.body_ref()) else {
        return false;
    };
    // Canonical wire order is outermost first, so the capture-site user frame
    // is the last frame.
    let crash_function = last_exception_stack_function(&body);

    crash_function.contains("capture_exception_sends_exception_event")
        && !crash_function.contains("Client::capture_exception")
        && !crash_function.contains("build_exception_event")
}

fn request_has_capture_exception_with_user_frame_last(req: &HttpMockRequest) -> bool {
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(req.body_ref()) else {
        return false;
    };
    let crash_function = last_exception_stack_function(&body);

    crash_function.contains("capture_exception_with_attaches_identity_and_context")
        && !crash_function.contains("Client::capture_exception")
        && !crash_function.contains("build_exception_event")
}

/// Ported from the removed V0 suite: an `$exception_list` entry is present but
/// carries no `stacktrace`, i.e. `capture_stacktrace(false)` was honored.
fn request_has_no_stacktrace(req: &HttpMockRequest) -> bool {
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(req.body_ref()) else {
        return false;
    };

    body.pointer("/batch/0/properties/$exception_list/0")
        .is_some()
        && body
            .pointer("/batch/0/properties/$exception_list/0/stacktrace")
            .is_none()
}

#[cfg(not(feature = "async-client"))]
mod blocking {
    use super::*;
    use posthog_rs::CaptureExceptionOptions;

    fn create_test_client(base_url: String) -> posthog_rs::Client {
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(base_url)
            .max_capture_attempts(1u32)
            .build()
            .unwrap();
        posthog_rs::client(options)
    }

    #[test]
    fn capture_exception_sends_exception_event() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .body_includes(r#""event":"$exception""#)
                .body_includes(r#""process_person_profile":false"#)
                .body_includes(r#""$exception_level":"error""#)
                .body_includes(r#""value":"payment failed""#)
                .body_includes(r#""platform":"native""#)
                .matches(request_has_capture_exception_user_frame_last);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(ok_response());
        });

        let client = create_test_client(server.base_url());
        client.capture_exception(&TestError).unwrap();
        client.flush();

        capture_mock.assert_hits(1);
    }

    #[test]
    fn capture_exception_with_attaches_identity_and_context() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .body_includes(r#""event":"$exception""#)
                .body_includes(r#""distinct_id":"user-1""#)
                .body_includes(r#""route":"/checkout""#)
                .body_includes(r#""$groups":{"company":"company-1"}"#)
                .body_includes(r#""$exception_fingerprint":"checkout-error""#)
                .body_includes(r#""$exception_level":"warning""#)
                .matches(request_has_capture_exception_with_user_frame_last);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(ok_response());
        });

        let client = create_test_client(server.base_url());
        client
            .capture_exception_with(
                &TestError,
                CaptureExceptionOptions::new()
                    .distinct_id("user-1")
                    .property("route", "/checkout")
                    .unwrap()
                    .group("company", "company-1")
                    .fingerprint("checkout-error")
                    .level("warning"),
            )
            .unwrap();
        client.flush();

        capture_mock.assert_hits(1);
    }

    #[test]
    fn disabled_capture_exception_does_not_build_exception_payload() {
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host("http://127.0.0.1:1")
            .disabled(true)
            .build()
            .unwrap();
        let client = posthog_rs::client(options);

        client.capture_exception(&PanicDisplayError).unwrap();
        client
            .capture_exception_with(
                &PanicDisplayError,
                CaptureExceptionOptions::new().distinct_id("user-1"),
            )
            .unwrap();
    }

    /// Ported from the removed V0 suite: client-level `ErrorTrackingOptions`
    /// (here `capture_stacktrace(false)`) apply to `capture_exception`.
    #[test]
    fn capture_exception_uses_client_error_tracking_options() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .body_includes(r#""event":"$exception""#)
                .body_includes(r#""value":"payment failed""#)
                .matches(request_has_no_stacktrace);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(ok_response());
        });

        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .max_capture_attempts(1u32)
            .error_tracking(
                posthog_rs::ErrorTrackingOptionsBuilder::default()
                    .capture_stacktrace(false)
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap();
        let client = posthog_rs::client(options);

        client.capture_exception(&TestError).unwrap();
        client.flush();

        capture_mock.assert_hits(1);
    }
}

#[cfg(feature = "async-client")]
mod async_client {
    use super::*;
    use posthog_rs::CaptureExceptionOptions;

    async fn create_test_client(base_url: String) -> posthog_rs::Client {
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(base_url)
            .max_capture_attempts(1u32)
            .build()
            .unwrap();
        posthog_rs::client(options).await
    }

    #[tokio::test]
    async fn capture_exception_sends_exception_event() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .body_includes(r#""event":"$exception""#)
                .body_includes(r#""process_person_profile":false"#)
                .body_includes(r#""$exception_level":"error""#)
                .body_includes(r#""value":"payment failed""#)
                .body_includes(r#""platform":"native""#)
                .matches(request_has_capture_exception_user_frame_last);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(ok_response());
        });

        let client = create_test_client(server.base_url()).await;
        client.capture_exception(&TestError).await.unwrap();
        client.flush().await;

        capture_mock.assert_hits(1);
    }

    #[tokio::test]
    async fn capture_exception_with_attaches_identity_and_context() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .body_includes(r#""event":"$exception""#)
                .body_includes(r#""distinct_id":"user-1""#)
                .body_includes(r#""route":"/checkout""#)
                .body_includes(r#""$groups":{"company":"company-1"}"#)
                .body_includes(r#""$exception_fingerprint":"checkout-error""#)
                .body_includes(r#""$exception_level":"warning""#)
                .matches(request_has_capture_exception_with_user_frame_last);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(ok_response());
        });

        let client = create_test_client(server.base_url()).await;
        client
            .capture_exception_with(
                &TestError,
                CaptureExceptionOptions::new()
                    .distinct_id("user-1")
                    .property("route", "/checkout")
                    .unwrap()
                    .group("company", "company-1")
                    .fingerprint("checkout-error")
                    .level("warning"),
            )
            .await
            .unwrap();
        client.flush().await;

        capture_mock.assert_hits(1);
    }

    #[tokio::test]
    async fn disabled_capture_exception_does_not_build_exception_payload() {
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host("http://127.0.0.1:1")
            .disabled(true)
            .build()
            .unwrap();
        let client = posthog_rs::client(options).await;

        client.capture_exception(&PanicDisplayError).await.unwrap();
        client
            .capture_exception_with(
                &PanicDisplayError,
                CaptureExceptionOptions::new().distinct_id("user-1"),
            )
            .await
            .unwrap();
    }

    /// Ported from the removed V0 suite: client-level `ErrorTrackingOptions`
    /// (here `capture_stacktrace(false)`) apply to `capture_exception`.
    #[tokio::test]
    async fn capture_exception_uses_client_error_tracking_options() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .body_includes(r#""event":"$exception""#)
                .body_includes(r#""value":"payment failed""#)
                .matches(request_has_no_stacktrace);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(ok_response());
        });

        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .max_capture_attempts(1u32)
            .error_tracking(
                posthog_rs::ErrorTrackingOptionsBuilder::default()
                    .capture_stacktrace(false)
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap();
        let client = posthog_rs::client(options).await;

        client.capture_exception(&TestError).await.unwrap();
        client.flush().await;

        capture_mock.assert_hits(1);
    }
}

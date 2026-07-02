// V1-transport twins of the Error Tracking integration tests in
// `test_error_tracking.rs`, which is v0-shaped and gated out under capture-v1.
// The mock responds with an empty results map and the client runs a single
// attempt, so each test asserts exactly one well-formed V1 request.
#![cfg(all(feature = "error-tracking", feature = "capture-v1"))]

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

const V1_CAPTURE_PATH: &str = "/i/v1/analytics/events";

fn v1_ok_response() -> serde_json::Value {
    json!({ "results": {} })
}

fn crash_site_stack_function(body: &serde_json::Value) -> &str {
    body.pointer("/batch/0/properties/$exception_list/0/stacktrace/frames")
        .and_then(|value| value.as_array())
        .and_then(|frames| frames.last())
        .and_then(|frame| frame.get("function"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
}

fn request_has_capture_exception_user_frame_at_crash_site(req: &HttpMockRequest) -> bool {
    let Some(body) = req.body.as_deref() else {
        return false;
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let crash_function = crash_site_stack_function(&body);

    crash_function.contains("capture_exception_sends_exception_event")
        && !crash_function.contains("Client::capture_exception")
        && !crash_function.contains("build_exception_event")
}

fn request_has_capture_exception_with_user_frame_at_crash_site(req: &HttpMockRequest) -> bool {
    let Some(body) = req.body.as_deref() else {
        return false;
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let crash_function = crash_site_stack_function(&body);

    crash_function.contains("capture_exception_with_attaches_identity_and_context")
        && !crash_function.contains("Client::capture_exception")
        && !crash_function.contains("build_exception_event")
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
                .path(V1_CAPTURE_PATH)
                .body_contains(r#""event":"$exception""#)
                .body_contains(r#""process_person_profile":false"#)
                .body_contains(r#""$exception_level":"error""#)
                .body_contains(r#""value":"payment failed""#)
                .body_contains(r#""platform":"native""#)
                .matches(request_has_capture_exception_user_frame_at_crash_site);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(v1_ok_response());
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
                .path(V1_CAPTURE_PATH)
                .body_contains(r#""event":"$exception""#)
                .body_contains(r#""distinct_id":"user-1""#)
                .body_contains(r#""route":"/checkout""#)
                .body_contains(r#""$groups":{"company":"company-1"}"#)
                .body_contains(r#""$exception_fingerprint":"checkout-error""#)
                .body_contains(r#""$exception_level":"warning""#)
                .matches(request_has_capture_exception_with_user_frame_at_crash_site);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(v1_ok_response());
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
                .path(V1_CAPTURE_PATH)
                .body_contains(r#""event":"$exception""#)
                .body_contains(r#""process_person_profile":false"#)
                .body_contains(r#""$exception_level":"error""#)
                .body_contains(r#""value":"payment failed""#)
                .body_contains(r#""platform":"native""#)
                .matches(request_has_capture_exception_user_frame_at_crash_site);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(v1_ok_response());
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
                .path(V1_CAPTURE_PATH)
                .body_contains(r#""event":"$exception""#)
                .body_contains(r#""distinct_id":"user-1""#)
                .body_contains(r#""route":"/checkout""#)
                .body_contains(r#""$groups":{"company":"company-1"}"#)
                .body_contains(r#""$exception_fingerprint":"checkout-error""#)
                .body_contains(r#""$exception_level":"warning""#)
                .matches(request_has_capture_exception_with_user_frame_at_crash_site);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(v1_ok_response());
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
}

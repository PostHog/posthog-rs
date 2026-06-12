// These tests assert the V0 wire shape; their capture-v1 twins live in
// `test_error_tracking_v1.rs`.
#![cfg(all(feature = "error-tracking", not(feature = "capture-v1")))]

use httpmock::prelude::*;
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

fn request_has_capture_exception_user_frame_first(req: &HttpMockRequest) -> bool {
    let Some(body) = req.body.as_deref() else {
        return false;
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let first_function = first_exception_stack_function(&body);

    first_function.contains("capture_exception_sends_exception_event")
        && !first_function.contains("Client::capture_exception")
        && !first_function.contains("build_exception_event")
}

fn request_has_capture_exception_with_user_frame_first(req: &HttpMockRequest) -> bool {
    let Some(body) = req.body.as_deref() else {
        return false;
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let first_function = first_exception_stack_function(&body);

    first_function.contains("capture_exception_with_attaches_identity_and_context")
        && !first_function.contains("Client::capture_exception")
        && !first_function.contains("build_exception_event")
}

fn request_has_no_stacktrace(req: &HttpMockRequest) -> bool {
    let Some(body) = req.body.as_deref() else {
        return false;
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };

    body.pointer("/properties/$exception_list/0").is_some()
        && body
            .pointer("/properties/$exception_list/0/stacktrace")
            .is_none()
}

fn first_exception_stack_function(body: &serde_json::Value) -> &str {
    body.pointer("/properties/$exception_list/0/stacktrace/frames")
        .and_then(|value| value.as_array())
        .and_then(|frames| frames.first())
        .and_then(|frame| frame.get("function"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
}

#[cfg(not(feature = "async-client"))]
mod blocking {
    use super::*;
    use posthog_rs::CaptureExceptionOptions;

    fn create_test_client(base_url: String) -> posthog_rs::Client {
        let options: posthog_rs::ClientOptions = ("test_api_key", base_url.as_str()).into();
        posthog_rs::client(options)
    }

    #[test]
    fn capture_exception_sends_exception_event() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v0/e/")
                .body_contains(r#""event":"$exception""#)
                .body_contains(r#""$process_person_profile":false"#)
                .body_contains(r#""$exception_level":"error""#)
                .body_contains(r#""value":"payment failed""#)
                .body_contains(r#""platform":"rust""#)
                .body_contains(r#""lang":"rust""#)
                .matches(request_has_capture_exception_user_frame_first);
            then.status(200);
        });

        let client = create_test_client(server.base_url());
        client.capture_exception(&TestError).unwrap();

        capture_mock.assert_hits(1);
    }

    #[test]
    fn capture_exception_with_attaches_identity_and_context() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v0/e/")
                .body_contains(r#""event":"$exception""#)
                .body_contains(r#""distinct_id":"user-1""#)
                .body_contains(r#""route":"/checkout""#)
                .body_contains(r#""$groups":{"company":"company-1"}"#)
                .body_contains(r#""$exception_fingerprint":"checkout-error""#)
                .body_contains(r#""$exception_level":"warning""#)
                .matches(request_has_capture_exception_with_user_frame_first);
            then.status(200);
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

    #[test]
    fn capture_exception_uses_client_error_tracking_options() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v0/e/")
                .body_contains(r#""event":"$exception""#)
                .body_contains(r#""value":"payment failed""#)
                .matches(request_has_no_stacktrace);
            then.status(200);
        });

        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
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

        capture_mock.assert_hits(1);
    }
}

#[cfg(feature = "async-client")]
mod async_client {
    use super::*;
    use posthog_rs::CaptureExceptionOptions;

    async fn create_test_client(base_url: String) -> posthog_rs::Client {
        let options: posthog_rs::ClientOptions = ("test_api_key", base_url.as_str()).into();
        posthog_rs::client(options).await
    }

    #[tokio::test]
    async fn capture_exception_sends_exception_event() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v0/e/")
                .body_contains(r#""event":"$exception""#)
                .body_contains(r#""$process_person_profile":false"#)
                .body_contains(r#""$exception_level":"error""#)
                .body_contains(r#""value":"payment failed""#)
                .body_contains(r#""platform":"rust""#)
                .body_contains(r#""lang":"rust""#)
                .matches(request_has_capture_exception_user_frame_first);
            then.status(200);
        });

        let client = create_test_client(server.base_url()).await;
        client.capture_exception(&TestError).await.unwrap();

        capture_mock.assert_hits(1);
    }

    #[tokio::test]
    async fn capture_exception_with_attaches_identity_and_context() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v0/e/")
                .body_contains(r#""event":"$exception""#)
                .body_contains(r#""distinct_id":"user-1""#)
                .body_contains(r#""route":"/checkout""#)
                .body_contains(r#""$groups":{"company":"company-1"}"#)
                .body_contains(r#""$exception_fingerprint":"checkout-error""#)
                .body_contains(r#""$exception_level":"warning""#)
                .matches(request_has_capture_exception_with_user_frame_first);
            then.status(200);
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

    #[tokio::test]
    async fn capture_exception_uses_client_error_tracking_options() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v0/e/")
                .body_contains(r#""event":"$exception""#)
                .body_contains(r#""value":"payment failed""#)
                .matches(request_has_no_stacktrace);
            then.status(200);
        });

        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
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

        capture_mock.assert_hits(1);
    }
}

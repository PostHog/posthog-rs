#![cfg(feature = "error-tracking")]

use httpmock::prelude::*;
use serde_json::json;
use std::error::Error as StdError;
use std::fmt;

use chrono::{Duration, Utc};

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

fn flags_response_fixture() -> serde_json::Value {
    json!({
        "featureFlags": {"checkout": true},
        "featureFlagPayloads": {}
    })
}

fn request_has_capture_error_user_frame_first(req: &HttpMockRequest) -> bool {
    let Some(body) = req.body.as_deref() else {
        return false;
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let first_function = first_exception_stack_function(&body);

    first_function.contains("capture_error_sends_exception_event")
        && !first_function.contains("Client::capture_error")
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
    use posthog_rs::{EvaluateFlagsOptions, ExceptionCapture};

    fn create_test_client(base_url: String) -> posthog_rs::Client {
        let options: posthog_rs::ClientOptions = ("test_api_key", base_url.as_str()).into();
        posthog_rs::client(options)
    }

    #[test]
    fn capture_error_sends_exception_event() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v0/e/")
                .body_contains(r#""event":"$exception""#)
                .body_contains(r#""distinct_id":"user-1""#)
                .body_contains(r#""$exception_level":"error""#)
                .body_contains(r#""value":"payment failed""#)
                .body_contains(r#""platform":"rust""#)
                .body_contains(r#""lang":"rust""#)
                .matches(request_has_capture_error_user_frame_first);
            then.status(200);
        });

        let client = create_test_client(server.base_url());
        client.capture_error(&TestError, "user-1").unwrap();

        capture_mock.assert_hits(1);
    }

    #[test]
    fn disabled_capture_error_does_not_build_exception_payload() {
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host("http://127.0.0.1:1")
            .disabled(true)
            .build()
            .unwrap();
        let client = posthog_rs::client(options);

        client.capture_error(&PanicDisplayError, "user-1").unwrap();
    }

    #[test]
    fn disabled_capture_exception_does_not_convert_payload() {
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host("http://127.0.0.1:1")
            .disabled(true)
            .build()
            .unwrap();
        let client = posthog_rs::client(options);
        let exception = ExceptionCapture::from_message("FutureError", "future event")
            .with_timestamp(Utc::now() + Duration::days(1));

        client.capture_exception(exception).unwrap();
    }

    #[test]
    fn exception_from_error_uses_client_error_tracking_options() {
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host("http://127.0.0.1:1")
            .error_tracking(
                posthog_rs::ErrorTrackingOptionsBuilder::default()
                    .capture_stacktrace(false)
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap();
        let client = posthog_rs::client(options);

        let event = client
            .exception_from_error(&TestError)
            .into_event()
            .unwrap();
        let json = serde_json::to_value(event).unwrap();

        assert!(json["properties"]["$exception_list"][0]
            .get("stacktrace")
            .is_none());
    }

    #[test]
    fn exception_from_error_skips_client_helper_frame() {
        let client = create_test_client("http://127.0.0.1:1".to_string());

        let event = client
            .exception_from_error(&TestError)
            .into_event()
            .unwrap();
        let json = serde_json::to_value(event).unwrap();
        let first_function = first_exception_stack_function(&json);

        assert!(
            first_function.contains("exception_from_error_skips_client_helper_frame"),
            "expected user frame first, got {:?}",
            first_function
        );
        assert!(
            !first_function.contains("Client::exception_from_error"),
            "expected SDK helper frame to be skipped, got {:?}",
            first_function
        );
    }

    #[test]
    fn capture_exception_attaches_custom_properties_groups_and_flags() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v0/e/")
                .body_contains(r#""route":"/checkout""#)
                .body_contains(r#""$groups":{"company":"company-1"}"#)
                .body_contains(r#""$feature/checkout":true"#)
                .body_contains(r#""$exception_fingerprint":"checkout-error""#);
            then.status(200);
        });

        let exception = ExceptionCapture::from_message("CheckoutError", "card declined")
            .with_distinct_id("user-1")
            .with_prop("route", "/checkout")
            .unwrap()
            .with_group("company", "company-1")
            .with_fingerprint("checkout-error");

        let client = create_test_client(server.base_url());
        let flags = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        let exception = exception.with_flags(&flags);
        client.capture_exception(exception).unwrap();

        capture_mock.assert_hits(1);
    }
}

#[cfg(feature = "async-client")]
mod async_client {
    use super::*;
    use posthog_rs::{EvaluateFlagsOptions, ExceptionCapture};

    async fn create_test_client(base_url: String) -> posthog_rs::Client {
        let options: posthog_rs::ClientOptions = ("test_api_key", base_url.as_str()).into();
        posthog_rs::client(options).await
    }

    #[tokio::test]
    async fn capture_error_sends_exception_event() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v0/e/")
                .body_contains(r#""event":"$exception""#)
                .body_contains(r#""distinct_id":"user-1""#)
                .body_contains(r#""$exception_level":"error""#)
                .body_contains(r#""value":"payment failed""#)
                .body_contains(r#""platform":"rust""#)
                .body_contains(r#""lang":"rust""#)
                .matches(request_has_capture_error_user_frame_first);
            then.status(200);
        });

        let client = create_test_client(server.base_url()).await;
        client.capture_error(&TestError, "user-1").await.unwrap();

        capture_mock.assert_hits(1);
    }

    #[tokio::test]
    async fn disabled_capture_error_does_not_build_exception_payload() {
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host("http://127.0.0.1:1")
            .disabled(true)
            .build()
            .unwrap();
        let client = posthog_rs::client(options).await;

        client
            .capture_error(&PanicDisplayError, "user-1")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn disabled_capture_exception_does_not_convert_payload() {
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host("http://127.0.0.1:1")
            .disabled(true)
            .build()
            .unwrap();
        let client = posthog_rs::client(options).await;
        let exception = ExceptionCapture::from_message("FutureError", "future event")
            .with_timestamp(Utc::now() + Duration::days(1));

        client.capture_exception(exception).await.unwrap();
    }

    #[tokio::test]
    async fn exception_from_error_uses_client_error_tracking_options() {
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host("http://127.0.0.1:1")
            .error_tracking(
                posthog_rs::ErrorTrackingOptionsBuilder::default()
                    .capture_stacktrace(false)
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap();
        let client = posthog_rs::client(options).await;

        let event = client
            .exception_from_error(&TestError)
            .into_event()
            .unwrap();
        let json = serde_json::to_value(event).unwrap();

        assert!(json["properties"]["$exception_list"][0]
            .get("stacktrace")
            .is_none());
    }

    #[tokio::test]
    async fn exception_from_error_skips_client_helper_frame() {
        let client = create_test_client("http://127.0.0.1:1".to_string()).await;

        let event = client
            .exception_from_error(&TestError)
            .into_event()
            .unwrap();
        let json = serde_json::to_value(event).unwrap();
        let first_function = first_exception_stack_function(&json);

        assert!(
            first_function.contains("exception_from_error_skips_client_helper_frame"),
            "expected user frame first, got {:?}",
            first_function
        );
        assert!(
            !first_function.contains("Client::exception_from_error"),
            "expected SDK helper frame to be skipped, got {:?}",
            first_function
        );
    }

    #[tokio::test]
    async fn capture_exception_attaches_custom_properties_groups_and_flags() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v0/e/")
                .body_contains(r#""route":"/checkout""#)
                .body_contains(r#""$groups":{"company":"company-1"}"#)
                .body_contains(r#""$feature/checkout":true"#)
                .body_contains(r#""$exception_fingerprint":"checkout-error""#);
            then.status(200);
        });

        let exception = ExceptionCapture::from_message("CheckoutError", "card declined")
            .with_distinct_id("user-1")
            .with_prop("route", "/checkout")
            .unwrap()
            .with_group("company", "company-1")
            .with_fingerprint("checkout-error");

        let client = create_test_client(server.base_url()).await;
        let flags = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        let exception = exception.with_flags(&flags);
        client.capture_exception(exception).await.unwrap();

        capture_mock.assert_hits(1);
    }
}

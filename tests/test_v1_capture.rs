#![cfg(feature = "async-client")]

use httpmock::prelude::*;
use posthog_rs::{CaptureMode, ClientOptionsBuilder, Event};
use serde_json::json;

async fn create_v1_client(base_url: String) -> posthog_rs::Client {
    let options = ClientOptionsBuilder::default()
        .api_key("phc_test_token".to_string())
        .host(base_url)
        .capture_mode(CaptureMode::V1)
        .max_capture_retries(3u32)
        .retry_initial_backoff_ms(10u64)
        .retry_max_backoff_ms(50u64)
        .build()
        .unwrap();
    posthog_rs::client(options).await
}

#[tokio::test]
async fn v1_capture_single_event_success() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header_exists("authorization")
            .header_exists("posthog-request-id")
            .header_exists("posthog-attempt")
            .header_exists("posthog-request-timestamp")
            .header_exists("posthog-sdk-info");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "results": {
                    "placeholder": { "result": "ok" }
                }
            }));
    });

    let client = create_v1_client(server.base_url()).await;
    let event = Event::new("test_event", "user-1");

    let result = client.capture(event).await;
    assert!(result.is_ok());
    mock.assert();
}

#[tokio::test]
async fn v1_capture_bearer_auth_header() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("authorization", "Bearer phc_test_token");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "results": {}
            }));
    });

    let client = create_v1_client(server.base_url()).await;
    client.capture(Event::new("test", "user-1")).await.unwrap();
    mock.assert();
}

#[tokio::test]
async fn v1_capture_retries_on_server_error() {
    let server = MockServer::start();

    let fail_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "1");
        then.status(503)
            .header("content-type", "application/json")
            .header("retry-after", "1")
            .json_body(json!({
                "error": "service_unavailable",
                "error_description": "Service Unavailable"
            }));
    });

    let success_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "2");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "results": {}
            }));
    });

    let client = create_v1_client(server.base_url()).await;
    let result = client.capture(Event::new("test", "user-1")).await;

    assert!(result.is_ok());
    fail_mock.assert();
    success_mock.assert();
}

#[tokio::test]
async fn v1_capture_does_not_retry_on_401() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(401)
            .header("content-type", "application/json")
            .json_body(json!({
                "error": "invalid_api_token",
                "error_description": "The provided API token is not valid."
            }));
    });

    let client = create_v1_client(server.base_url()).await;
    let result = client.capture(Event::new("test", "user-1")).await;

    assert!(result.is_err());
    mock.assert_hits(1);
}

#[tokio::test]
async fn v1_capture_does_not_retry_on_402() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(402)
            .header("content-type", "application/json")
            .json_body(json!({
                "error": "billing_limit_exceeded",
                "error_description": "Billing quota exceeded."
            }));
    });

    let client = create_v1_client(server.base_url()).await;
    let result = client.capture(Event::new("test", "user-1")).await;

    assert!(matches!(
        result,
        Err(posthog_rs::Error::BillingLimitExceeded(_))
    ));
    mock.assert_hits(1);
}

#[tokio::test]
async fn v1_capture_partial_batch_retry() {
    let server = MockServer::start();

    let mut event1 = Event::new("event_1", "user-1");
    let mut event2 = Event::new("event_2", "user-1");

    let uuid1 = uuid::Uuid::now_v7();
    let uuid2 = uuid::Uuid::now_v7();
    event1.set_uuid(uuid1);
    event2.set_uuid(uuid2);

    let uuid1_str = uuid1.to_string();
    let uuid2_str = uuid2.to_string();
    let uuid2_str_clone = uuid2_str.clone();

    let first_resp = json!({
        "results": {
            &uuid1_str: { "result": "ok" },
            &uuid2_str: { "result": "retry", "details": "not_persisted" }
        }
    });

    let retry_resp = json!({
        "results": {
            &uuid2_str_clone: { "result": "ok" }
        }
    });

    let first_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "1");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(first_resp);
    });

    let retry_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "2");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(retry_resp);
    });

    let client = create_v1_client(server.base_url()).await;
    let result = client.capture_batch(vec![event1, event2], false).await;

    assert!(result.is_ok());
    first_mock.assert();
    retry_mock.assert();
}

#[tokio::test]
async fn v1_capture_exhausts_retries() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(500)
            .header("content-type", "application/json")
            .json_body(json!({
                "error": "internal_error",
                "error_description": "Internal Server Error"
            }));
    });

    let client = create_v1_client(server.base_url()).await;
    let result = client.capture(Event::new("test", "user-1")).await;

    assert!(result.is_err());
    mock.assert_hits(3);
}

#[tokio::test]
async fn v1_capture_sends_event_options() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "results": {}
            }));
    });

    let client = create_v1_client(server.base_url()).await;
    let mut event = Event::new("test", "user-1");
    event.set_options(|opts| {
        opts.cookieless_mode = true;
        opts.process_person_profile = false;
    });

    client.capture(event).await.unwrap();
    mock.assert();
}

#[tokio::test]
async fn v1_capture_disabled_client_noop() {
    let options = ClientOptionsBuilder::default()
        .api_key("phc_test".to_string())
        .capture_mode(CaptureMode::V1)
        .disabled(true)
        .build()
        .unwrap();
    let client = posthog_rs::client(options).await;

    let result = client.capture(Event::new("test", "user-1")).await;
    assert!(result.is_ok());
}

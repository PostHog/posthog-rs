#![cfg(not(feature = "async-client"))]

use httpmock::prelude::*;
use posthog_rs::{CaptureMode, ClientOptionsBuilder, Event};
use serde_json::json;

fn create_v1_client(base_url: String) -> posthog_rs::Client {
    let options = ClientOptionsBuilder::default()
        .api_key("phc_test_token".to_string())
        .host(base_url)
        .capture_mode(CaptureMode::V1)
        .max_capture_retries(3u32)
        .retry_initial_backoff_ms(10u64)
        .retry_max_backoff_ms(50u64)
        .build()
        .unwrap();
    posthog_rs::client(options)
}

#[test]
fn v1_blocking_capture_success() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("authorization", "Bearer phc_test_token")
            .header_exists("posthog-request-id")
            .header_exists("posthog-attempt");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    });

    let client = create_v1_client(server.base_url());
    let result = client.capture(Event::new("test_event", "user-1"));
    assert!(result.is_ok());
    mock.assert();
}

#[test]
fn v1_blocking_retries_on_server_error() {
    let server = MockServer::start();

    let fail_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "1");
        then.status(500).json_body(json!({
            "error": "internal_error",
            "error_description": "Internal Server Error"
        }));
    });

    let success_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "2");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    });

    let client = create_v1_client(server.base_url());
    let result = client.capture(Event::new("test", "user-1"));

    assert!(result.is_ok());
    fail_mock.assert();
    success_mock.assert();
}

#[test]
fn v1_blocking_does_not_retry_on_401() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(401).body("Unauthorized");
    });

    let client = create_v1_client(server.base_url());
    let result = client.capture(Event::new("test", "user-1"));

    assert!(result.is_err());
    mock.assert_hits(1);
}

#[test]
fn v1_blocking_partial_batch_retry() {
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

    let client = create_v1_client(server.base_url());
    let result = client.capture_batch(vec![event1, event2], false);

    assert!(result.is_ok());
    first_mock.assert();
    retry_mock.assert();
}

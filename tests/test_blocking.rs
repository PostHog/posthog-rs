#![cfg(all(not(feature = "async-client"), not(feature = "capture-v1")))]

mod common;

use common::default_user_agent;
use httpmock::prelude::*;
use reqwest::header::USER_AGENT;
use serde_json::json;

fn create_test_client(base_url: String) -> posthog_rs::Client {
    let options: posthog_rs::ClientOptions = ("test_api_key", base_url.as_str()).into();
    posthog_rs::client(options)
}

#[cfg(not(feature = "capture-v1"))]
#[test]
fn test_client_with_empty_api_key_is_noop() {
    for api_key in [None, Some(" \n\t ")] {
        assert_disabled_client_is_noop(api_key);
    }
}

#[cfg(not(feature = "capture-v1"))]
fn assert_disabled_client_is_noop(api_key: Option<&str>) {
    let server = MockServer::start();

    let capture_mock = server.mock(|when, then| {
        when.method(POST).path("/i/v0/e/");
        then.status(200);
    });
    let batch_mock = server.mock(|when, then| {
        when.method(POST).path("/batch/");
        then.status(200);
    });
    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({
            "featureFlags": {},
            "featureFlagPayloads": {}
        }));
    });

    let mut options_builder = posthog_rs::ClientOptionsBuilder::default();
    if let Some(api_key) = api_key {
        options_builder.api_key(api_key.to_string());
    }
    let options = options_builder.host(server.base_url()).build().unwrap();
    assert!(options.is_disabled());

    let client = posthog_rs::client(options);
    let event = posthog_rs::Event::new("test_event", "user1");

    client.capture(event.clone());
    client.capture_batch(vec![event], false);

    let flags = client
        .evaluate_flags("test-user", posthog_rs::EvaluateFlagsOptions::default())
        .unwrap();
    assert!(flags.keys().is_empty());

    capture_mock.assert_hits(0);
    batch_mock.assert_hits(0);
    flags_mock.assert_hits(0);
}

#[cfg(not(feature = "capture-v1"))]
#[test]
fn test_capture_batch_empty_is_noop() {
    let server = MockServer::start();

    let batch_mock = server.mock(|when, then| {
        when.method(POST).path("/batch/");
        then.status(200).body("ok");
    });

    let client = create_test_client(server.base_url());
    client.capture_batch(vec![], false);

    batch_mock.assert_hits(0);
}

#[cfg(not(feature = "capture-v1"))]
#[test]
fn test_capture_batch_sends_to_batch_endpoint() {
    let server = MockServer::start();

    let batch_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/batch/")
            .header(USER_AGENT.to_string(), default_user_agent())
            .body_includes(r#""historical_migration":false"#);
        then.status(200);
    });

    let client = create_test_client(server.base_url());

    let event = posthog_rs::Event::new("test_event", "user1");
    client.capture_batch(vec![event], false);
    client.flush();

    batch_mock.assert();
}

#[cfg(not(feature = "capture-v1"))]
#[test]
fn test_capture_batch_historical_migration() {
    let server = MockServer::start();

    let batch_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/batch/")
            .body_includes(r#""historical_migration":true"#);
        then.status(200);
    });

    let client = create_test_client(server.base_url());

    let event = posthog_rs::Event::new("test_event", "user1");
    client.capture_batch(vec![event], true);
    client.flush();

    batch_mock.assert();
}

#[cfg(not(feature = "capture-v1"))]
#[test]
fn test_capture_batch_rate_limit() {
    let server = MockServer::start();

    let batch_mock = server.mock(|when, then| {
        when.method(POST).path("/batch/");
        then.status(429);
    });

    let client = create_test_client(server.base_url());

    let event = posthog_rs::Event::new("test_event", "user1");
    // Capture is now infallible; a terminal 429 is attempted once on flush and
    // then dropped (the rate-limit is logged, not returned to the caller).
    client.capture_batch(vec![event], true);
    client.flush();

    batch_mock.assert();
}

#[cfg(not(feature = "capture-v1"))]
#[test]
fn test_capture_batch_bad_request() {
    let server = MockServer::start();

    let batch_mock = server.mock(|when, then| {
        when.method(POST).path("/batch/");
        then.status(400).body("invalid payload");
    });

    let client = create_test_client(server.base_url());

    let event = posthog_rs::Event::new("test_event", "user1");
    // Capture is now infallible; a terminal 400 is attempted once on flush and
    // then dropped (the bad-request is logged, not returned to the caller).
    client.capture_batch(vec![event], false);
    client.flush();

    batch_mock.assert();
}

#[cfg(not(feature = "capture-v1"))]
#[test]
fn v0_capture_injects_is_server_by_default() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/batch/")
            .header(USER_AGENT.to_string(), default_user_agent())
            .body_includes("\"$is_server\":true");
        then.status(200).body("ok");
    });

    let client = create_test_client(server.base_url());
    let event = posthog_rs::Event::new("test_event", "user-1");
    client.capture(event);
    client.flush();
    mock.assert();
}

#[cfg(not(feature = "capture-v1"))]
#[test]
fn v0_capture_caller_override_wins_for_is_server() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/batch/")
            .body_includes("\"$is_server\":false");
        then.status(200).body("ok");
    });

    let client = create_test_client(server.base_url());
    let mut event = posthog_rs::Event::new("test_event", "user-1");
    event.insert_prop("$is_server", false).unwrap();
    client.capture(event);
    client.flush();
    mock.assert();
}

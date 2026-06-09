#![cfg(not(feature = "async-client"))]
// These tests exercise the legacy single-flag methods (`get_feature_flag`,
// `is_feature_enabled`, `get_feature_flag_payload`) that were deprecated in
// favor of `evaluate_flags()`. We keep the coverage in place during the
// deprecation window — tests for `evaluate_flags()` live in
// `tests/test_evaluate_flags.rs`.
#![allow(deprecated)]

use httpmock::prelude::*;
use posthog_rs::{get_default_user_agent, FlagValue};
use reqwest::header::USER_AGENT;
use serde_json::json;
use std::collections::HashMap;

fn create_test_client(base_url: String) -> posthog_rs::Client {
    // Use the From implementation to ensure endpoint_manager is set up correctly
    let options: posthog_rs::ClientOptions = (("test_api_key", base_url.as_str())).into();
    posthog_rs::client(options)
}

#[test]
fn test_get_all_feature_flags() {
    let server = MockServer::start();

    let mock_response = json!({
        "featureFlags": {
            "test-flag": true,
            "disabled-flag": false,
            "variant-flag": "control"
        },
        "featureFlagPayloads": {
            "variant-flag": {
                "color": "blue",
                "size": "large"
            }
        }
    });

    let flags_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/flags/")
            .query_param("v", "2")
            .json_body(json!({
                "api_key": "test_api_key",
                "distinct_id": "test-user"
            }));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(mock_response);
    });

    let client = create_test_client(server.base_url());

    let result = client.get_feature_flags("test-user".to_string(), None, None, None);

    assert!(result.is_ok());
    let (feature_flags, payloads) = result.unwrap();

    assert_eq!(
        feature_flags.get("test-flag"),
        Some(&FlagValue::Boolean(true))
    );
    assert_eq!(
        feature_flags.get("disabled-flag"),
        Some(&FlagValue::Boolean(false))
    );
    assert_eq!(
        feature_flags.get("variant-flag"),
        Some(&FlagValue::String("control".to_string()))
    );

    assert!(payloads.contains_key("variant-flag"));

    flags_mock.assert();
}

#[test]
fn test_sends_default_useragent() {
    let server = MockServer::start();

    let mock_response = json!({});

    let flags_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/flags/")
            .header(USER_AGENT.to_string(), get_default_user_agent())
            .query_param("v", "2");

        then.status(200)
            .header("content-type", "application/json")
            .json_body(mock_response);
    });

    let client = create_test_client(server.base_url());

    let _ = client.get_feature_flags("test-user".to_string(), None, None, None);
    flags_mock.assert();
}

#[test]
fn test_sends_custom_useragent() {
    let server = MockServer::start();

    let mock_response = json!({});
    let custom_user_agent = "custom-user-agent";

    let flags_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/flags/")
            .header(USER_AGENT.to_string(), custom_user_agent)
            .query_param("v", "2");

        then.status(200)
            .header("content-type", "application/json")
            .json_body(mock_response);
    });

    let options = posthog_rs::ClientOptionsBuilder::default()
        .api_key("test_api_key".into())
        .host(server.base_url().as_str())
        .user_agent("custom-user-agent".to_string())
        .build()
        .expect("should build client options");
    let client = posthog_rs::client(options);

    let _ = client.get_feature_flags("test-user".to_string(), None, None, None);
    flags_mock.assert();
}

#[test]
fn test_is_feature_enabled() {
    let server = MockServer::start();

    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({
            "featureFlags": {
                "enabled-flag": true,
                "disabled-flag": false
            },
            "featureFlagPayloads": {}
        }));
    });

    let client = create_test_client(server.base_url());

    let enabled_result = client.is_feature_enabled(
        "enabled-flag".to_string(),
        "test-user".to_string(),
        None,
        None,
        None,
    );

    assert!(enabled_result.is_ok());
    assert_eq!(enabled_result.unwrap(), true);

    let disabled_result = client.is_feature_enabled(
        "disabled-flag".to_string(),
        "test-user".to_string(),
        None,
        None,
        None,
    );

    assert!(disabled_result.is_ok());
    assert_eq!(disabled_result.unwrap(), false);

    flags_mock.assert_hits(2);
}

#[test]
fn test_get_feature_flag_with_properties() {
    let server = MockServer::start();

    let person_properties = json!({
        "country": "US",
        "age": 25,
        "plan": "premium"
    });

    let flags_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/flags/")
            .query_param("v", "2")
            .json_body(json!({
                "api_key": "test_api_key",
                "distinct_id": "test-user",
                "person_properties": person_properties
            }));
        then.status(200).json_body(json!({
            "featureFlags": {
                "premium-feature": true
            },
            "featureFlagPayloads": {}
        }));
    });

    let client = create_test_client(server.base_url());

    let mut props = HashMap::new();
    props.insert("country".to_string(), json!("US"));
    props.insert("age".to_string(), json!(25));
    props.insert("plan".to_string(), json!("premium"));

    let result = client.get_feature_flag(
        "premium-feature".to_string(),
        "test-user".to_string(),
        None,
        Some(props),
        None,
    );

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(FlagValue::Boolean(true)));

    flags_mock.assert();
}

#[test]
fn test_multivariate_flag() {
    let server = MockServer::start();

    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({
            "featureFlags": {
                "experiment": "variant-b"
            },
            "featureFlagPayloads": {}
        }));
    });

    let client = create_test_client(server.base_url());

    let result = client.get_feature_flag(
        "experiment".to_string(),
        "test-user".to_string(),
        None,
        None,
        None,
    );

    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        Some(FlagValue::String("variant-b".to_string()))
    );

    let enabled_result = client.is_feature_enabled(
        "experiment".to_string(),
        "test-user".to_string(),
        None,
        None,
        None,
    );

    assert!(enabled_result.is_ok());
    assert_eq!(enabled_result.unwrap(), true);

    flags_mock.assert_hits(2);
}

#[test]
fn test_api_error_handling() {
    let server = MockServer::start();

    let error_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(500).body("Internal Server Error");
    });

    let client = create_test_client(server.base_url());

    let result = client.get_feature_flags("test-user".to_string(), None, None, None);

    assert!(result.is_err());
    let error = result.unwrap_err();
    assert!(error.to_string().contains("500"));

    error_mock.assert();
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

    assert!(client.capture(event.clone()).is_ok());
    assert!(client.capture_batch(vec![event], false).is_ok());

    let (feature_flags, payloads) = client
        .get_feature_flags("test-user".to_string(), None, None, None)
        .unwrap();
    assert!(feature_flags.is_empty());
    assert!(payloads.is_empty());

    assert_eq!(
        client
            .get_feature_flag("test-flag", "test-user", None, None, None)
            .unwrap(),
        None
    );
    assert!(!client
        .is_feature_enabled("test-flag", "test-user", None, None, None)
        .unwrap());
    assert_eq!(
        client
            .get_feature_flag_payload("test-flag", "test-user")
            .unwrap(),
        None
    );

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
    let result = client.capture_batch(vec![], false);

    assert!(result.is_ok());
    batch_mock.assert_hits(0);
}

#[cfg(not(feature = "capture-v1"))]
#[test]
fn test_capture_batch_sends_to_batch_endpoint() {
    let server = MockServer::start();

    let batch_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/batch/")
            .body_contains(r#""historical_migration":false"#);
        then.status(200);
    });

    let client = create_test_client(server.base_url());

    let event = posthog_rs::Event::new("test_event", "user1");
    let result = client.capture_batch(vec![event], false);

    assert!(result.is_ok());
    batch_mock.assert();
}

#[cfg(not(feature = "capture-v1"))]
#[test]
fn test_capture_batch_historical_migration() {
    let server = MockServer::start();

    let batch_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/batch/")
            .body_contains(r#""historical_migration":true"#);
        then.status(200);
    });

    let client = create_test_client(server.base_url());

    let event = posthog_rs::Event::new("test_event", "user1");
    let result = client.capture_batch(vec![event], true);

    assert!(result.is_ok());
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
    let result = client.capture_batch(vec![event], true);

    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), posthog_rs::Error::RateLimit));
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
    let result = client.capture_batch(vec![event], false);

    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        posthog_rs::Error::BadRequest(msg) => assert_eq!(msg, "invalid payload"),
        other => panic!("expected BadRequest, got: {:?}", other),
    }
    batch_mock.assert();
}

#[cfg(not(feature = "capture-v1"))]
#[test]
fn v0_capture_injects_is_server_by_default() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v0/e/")
            .body_contains("\"$is_server\":true");
        then.status(200).body("ok");
    });

    let client = create_test_client(server.base_url());
    let event = posthog_rs::Event::new("test_event", "user-1");
    client.capture(event).unwrap();
    mock.assert();
}

#[cfg(not(feature = "capture-v1"))]
#[test]
fn v0_capture_caller_override_wins_for_is_server() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v0/e/")
            .body_contains("\"$is_server\":false");
        then.status(200).body("ok");
    });

    let client = create_test_client(server.base_url());
    let mut event = posthog_rs::Event::new("test_event", "user-1");
    event.insert_prop("$is_server", false).unwrap();
    client.capture(event).unwrap();
    mock.assert();
}

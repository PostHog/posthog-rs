#![cfg(not(feature = "async-client"))]

use httpmock::prelude::*;
use posthog_rs::FlagValue;
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

#[test]
fn test_capture_batch_rate_limit() {
    let server = MockServer::start();

    let batch_mock = server.mock(|when, then| {
        when.method(POST).path("/batch/");
        then.status(429).header("Retry-After", "60");
    });

    let client = create_test_client(server.base_url());

    let event = posthog_rs::Event::new("test_event", "user1");
    let result = client.capture_batch(vec![event], true);

    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        posthog_rs::Error::RateLimit { retry_after } => {
            assert_eq!(retry_after, Some(std::time::Duration::from_secs(60)));
        }
        other => panic!("expected RateLimit, got: {:?}", other),
    }
    batch_mock.assert();
}

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

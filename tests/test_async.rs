#![cfg(feature = "async-client")]

use httpmock::prelude::*;
use posthog_rs::{ClientOptionsBuilder, FlagValue};
use serde_json::json;
use std::collections::HashMap;

async fn create_test_client(base_url: String) -> posthog_rs::Client {
    let options = ClientOptionsBuilder::default()
        .api_endpoint(format!("{}/i/v0/e/", base_url))
        .api_key("test_api_key".to_string())
        .build()
        .unwrap();

    posthog_rs::client(options).await
}

#[tokio::test]
async fn test_get_all_feature_flags() {
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

    let decide_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/decide/")
            .query_param("v", "3")
            .json_body(json!({
                "api_key": "test_api_key",
                "distinct_id": "test-user"
            }));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(mock_response);
    });

    let client = create_test_client(server.base_url()).await;

    let result = client
        .get_feature_flags("test-user".to_string(), None, None, None)
        .await;

    assert!(result.is_ok());
    let response = result.unwrap();

    assert_eq!(
        response.feature_flags.get("test-flag"),
        Some(&FlagValue::Boolean(true))
    );
    assert_eq!(
        response.feature_flags.get("disabled-flag"),
        Some(&FlagValue::Boolean(false))
    );
    assert_eq!(
        response.feature_flags.get("variant-flag"),
        Some(&FlagValue::String("control".to_string()))
    );

    assert!(response.feature_flag_payloads.contains_key("variant-flag"));

    decide_mock.assert();
}

#[tokio::test]
async fn test_is_feature_enabled() {
    let server = MockServer::start();

    let decide_mock = server.mock(|when, then| {
        when.method(POST).path("/decide/").query_param("v", "3");
        then.status(200).json_body(json!({
            "featureFlags": {
                "enabled-flag": true,
                "disabled-flag": false
            },
            "featureFlagPayloads": {}
        }));
    });

    let client = create_test_client(server.base_url()).await;

    let enabled_result = client
        .is_feature_enabled(
            "enabled-flag".to_string(),
            "test-user".to_string(),
            None,
            None,
            None,
        )
        .await;

    assert!(enabled_result.is_ok());
    assert_eq!(enabled_result.unwrap(), true);

    let disabled_result = client
        .is_feature_enabled(
            "disabled-flag".to_string(),
            "test-user".to_string(),
            None,
            None,
            None,
        )
        .await;

    assert!(disabled_result.is_ok());
    assert_eq!(disabled_result.unwrap(), false);

    decide_mock.assert_hits(2);
}

#[tokio::test]
async fn test_get_feature_flag_with_properties() {
    let server = MockServer::start();

    let person_properties = json!({
        "country": "US",
        "age": 25,
        "plan": "premium"
    });

    let decide_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/decide/")
            .query_param("v", "3")
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

    let client = create_test_client(server.base_url()).await;

    let mut props = HashMap::new();
    props.insert("country".to_string(), json!("US"));
    props.insert("age".to_string(), json!(25));
    props.insert("plan".to_string(), json!("premium"));

    let result = client
        .get_feature_flag(
            "premium-feature".to_string(),
            "test-user".to_string(),
            None,
            Some(props),
            None,
        )
        .await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(FlagValue::Boolean(true)));

    decide_mock.assert();
}

#[tokio::test]
async fn test_multivariate_flag() {
    let server = MockServer::start();

    let decide_mock = server.mock(|when, then| {
        when.method(POST).path("/decide/").query_param("v", "3");
        then.status(200).json_body(json!({
            "featureFlags": {
                "experiment": "variant-b"
            },
            "featureFlagPayloads": {}
        }));
    });

    let client = create_test_client(server.base_url()).await;

    let result = client
        .get_feature_flag(
            "experiment".to_string(),
            "test-user".to_string(),
            None,
            None,
            None,
        )
        .await;

    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        Some(FlagValue::String("variant-b".to_string()))
    );

    let enabled_result = client
        .is_feature_enabled(
            "experiment".to_string(),
            "test-user".to_string(),
            None,
            None,
            None,
        )
        .await;

    assert!(enabled_result.is_ok());
    assert_eq!(enabled_result.unwrap(), true);

    decide_mock.assert_hits(2);
}

#[tokio::test]
async fn test_api_error_handling() {
    let server = MockServer::start();

    let error_mock = server.mock(|when, then| {
        when.method(POST).path("/decide/").query_param("v", "3");
        then.status(500).body("Internal Server Error");
    });

    let client = create_test_client(server.base_url()).await;

    let result = client
        .get_feature_flags("test-user".to_string(), None, None, None)
        .await;

    assert!(result.is_err());
    let error = result.unwrap_err();
    assert!(error.to_string().contains("500"));

    error_mock.assert();
}

#[tokio::test]
async fn test_get_feature_flag_payload() {
    let server = MockServer::start();

    let payload_data = json!({
        "steps": ["welcome", "profile", "preferences"],
        "theme": "dark"
    });

    let decide_mock = server.mock(|when, then| {
        when.method(POST).path("/decide/").query_param("v", "3");
        then.status(200).json_body(json!({
            "featureFlags": {
                "onboarding-flow": "variant-a"
            },
            "featureFlagPayloads": {
                "onboarding-flow": payload_data
            }
        }));
    });

    let client = create_test_client(server.base_url()).await;

    let result = client
        .get_feature_flag_payload("onboarding-flow".to_string(), "test-user".to_string())
        .await;

    assert!(result.is_ok());
    let payload = result.unwrap();
    assert!(payload.is_some());

    let payload_value = payload.unwrap();
    assert_eq!(payload_value["theme"], "dark");
    assert!(payload_value["steps"].is_array());

    decide_mock.assert();
}

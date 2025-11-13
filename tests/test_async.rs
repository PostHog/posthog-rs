#![cfg(feature = "async-client")]

use httpmock::prelude::*;
use posthog_rs::FlagValue;
use serde_json::json;
use std::collections::HashMap;

async fn create_test_client(base_url: String) -> posthog_rs::Client {
    // Use the From implementation to ensure endpoint_manager is set up correctly
    let options: posthog_rs::ClientOptions = (("test_api_key", base_url.as_str())).into();
    posthog_rs::client(options).await
}

#[tokio::test]
async fn test_get_all_feature_flags() {
    let server = MockServer::start();

    let mock_response = json!({
        "flags": {
            "test-flag": {
                "key": "test-flag",
                "enabled": true,
                "variant": null
            },
            "disabled-flag": {
                "key": "disabled-flag",
                "enabled": false,
                "variant": null
            },
            "variant-flag": {
                "key": "variant-flag",
                "enabled": true,
                "variant": "control",
                "metadata": {
                    "id": 1,
                    "version": 1,
                    "payload": "{\"color\": \"blue\", \"size\": \"large\"}"
                }
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

    let client = create_test_client(server.base_url()).await;

    let result = client
        .get_feature_flags("test-user".to_string(), None, None, None)
        .await;

    if let Err(e) = &result {
        eprintln!("Error: {:?}", e);
        eprintln!("Mock server URL: {}", server.base_url());
    }
    assert!(result.is_ok());
    let (feature_flags, payloads, _request_id, _flag_details) = result.unwrap();

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

#[tokio::test]
async fn test_is_feature_enabled() {
    let server = MockServer::start();

    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({
            "flags": {
                "enabled-flag": {
                    "key": "enabled-flag",
                    "enabled": true,
                    "variant": null
                },
                "disabled-flag": {
                    "key": "disabled-flag",
                    "enabled": false,
                    "variant": null
                }
            }
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

    flags_mock.assert_hits(2);
}

#[tokio::test]
async fn test_get_feature_flag_with_properties() {
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
            "flags": {
                "premium-feature": {
                    "key": "premium-feature",
                    "enabled": true,
                    "variant": null
                }
            }
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

    flags_mock.assert();
}

#[tokio::test]
async fn test_multivariate_flag() {
    let server = MockServer::start();

    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({
            "flags": {
                "experiment": {
                    "key": "experiment",
                    "enabled": true,
                    "variant": "variant-b"
                }
            }
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

    flags_mock.assert_hits(2);
}

#[tokio::test]
async fn test_api_error_handling() {
    let server = MockServer::start();

    let error_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
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

    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({
            "flags": {
                "onboarding-flow": {
                    "key": "onboarding-flow",
                    "enabled": true,
                    "variant": "variant-a",
                    "metadata": {
                        "id": 1,
                        "version": 1,
                        "payload": payload_data
                    }
                }
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

    flags_mock.assert();
}

#[tokio::test]
async fn test_nonexistent_flag() {
    let server = MockServer::start();

    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({
            "flags": {}
        }));
    });

    let client = create_test_client(server.base_url()).await;

    let result = client
        .get_feature_flag(
            "nonexistent-flag".to_string(),
            "test-user".to_string(),
            None,
            None,
            None,
        )
        .await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), None);

    let enabled_result = client
        .is_feature_enabled(
            "nonexistent-flag".to_string(),
            "test-user".to_string(),
            None,
            None,
            None,
        )
        .await;

    assert!(enabled_result.is_ok());
    assert_eq!(enabled_result.unwrap(), false);

    flags_mock.assert_hits(2);
}

#[tokio::test]
async fn test_empty_distinct_id() {
    let server = MockServer::start();

    let flags_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/flags/")
            .query_param("v", "2")
            .json_body(json!({
                "api_key": "test_api_key",
                "distinct_id": ""
            }));
        then.status(200).json_body(json!({
            "flags": {
                "test-flag": {
                    "key": "test-flag",
                    "enabled": true,
                    "variant": null
                }
            }
        }));
    });

    let client = create_test_client(server.base_url()).await;

    let result = client
        .get_feature_flag("test-flag".to_string(), "".to_string(), None, None, None)
        .await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(FlagValue::Boolean(true)));

    flags_mock.assert();
}

#[tokio::test]
async fn test_groups_parameter() {
    let server = MockServer::start();

    let groups_json = json!({
        "company": "acme-corp",
        "team": "engineering"
    });

    let flags_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/flags/")
            .query_param("v", "2")
            .json_body(json!({
                "api_key": "test_api_key",
                "distinct_id": "test-user",
                "groups": groups_json
            }));
        then.status(200).json_body(json!({
            "flags": {
                "team-feature": {
                    "key": "team-feature",
                    "enabled": true,
                    "variant": null
                }
            }
        }));
    });

    let client = create_test_client(server.base_url()).await;

    let mut groups = HashMap::new();
    groups.insert("company".to_string(), "acme-corp".to_string());
    groups.insert("team".to_string(), "engineering".to_string());

    let result = client
        .get_feature_flag(
            "team-feature".to_string(),
            "test-user".to_string(),
            Some(groups),
            None,
            None,
        )
        .await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(FlagValue::Boolean(true)));

    flags_mock.assert();
}

#[tokio::test]
async fn test_malformed_response() {
    let server = MockServer::start();

    let malformed_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).body("not json");
    });

    let client = create_test_client(server.base_url()).await;

    let result = client
        .get_feature_flags("test-user".to_string(), None, None, None)
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("expected"));

    malformed_mock.assert();
}

// Feature Flag Event Tests

#[tokio::test]
async fn test_feature_flag_event_captured() {
    // Test that $feature_flag_called event is captured when calling get_feature_flag
    let server = MockServer::start();

    // Mock the flags endpoint
    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({
            "flags": {
                "test-flag": {
                    "key": "test-flag",
                    "enabled": true,
                    "variant": null
                }
            }
        }));
    });

    // Mock the capture endpoint to verify event is sent
    let capture_mock = server.mock(|when, then| {
        when.method(POST).path("/i/v0/e/").json_body_partial(
            json!({
                "event": "$feature_flag_called"
            })
            .to_string(),
        );
        then.status(200);
    });

    let client = create_test_client(server.base_url()).await;

    let result = client
        .get_feature_flag("test-flag", "test-user", None, None, None)
        .await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(FlagValue::Boolean(true)));
    flags_mock.assert();
    capture_mock.assert();
}

#[tokio::test]
async fn test_feature_flag_event_deduplication() {
    // Test that calling same flag for same user doesn't send duplicate events
    let server = MockServer::start();

    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({
            "flags": {
                "test-flag": {
                    "key": "test-flag",
                    "enabled": true,
                    "variant": null
                }
            }
        }));
    });

    let capture_mock = server.mock(|when, then| {
        when.method(POST).path("/i/v0/e/");
        then.status(200);
    });

    let client = create_test_client(server.base_url()).await;

    // First call - should capture event
    client
        .get_feature_flag("test-flag", "test-user", None, None, None)
        .await
        .ok();

    // Second call - should NOT capture event (deduplication)
    client
        .get_feature_flag("test-flag", "test-user", None, None, None)
        .await
        .ok();

    flags_mock.assert_hits(2);
    capture_mock.assert_hits(1); // Only 1 event captured, not 2
}

#[tokio::test]
async fn test_feature_flag_event_different_user() {
    // Test that calling same flag for different user captures new event
    let server = MockServer::start();

    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({
            "flags": {
                "test-flag": {
                    "key": "test-flag",
                    "enabled": true,
                    "variant": null
                }
            }
        }));
    });

    let capture_mock = server.mock(|when, then| {
        when.method(POST).path("/i/v0/e/").json_body_partial(
            json!({
                "event": "$feature_flag_called"
            })
            .to_string(),
        );
        then.status(200);
    });

    let client = create_test_client(server.base_url()).await;

    // Call for user1 - should capture event
    client
        .get_feature_flag("test-flag", "user1", None, None, None)
        .await
        .ok();

    // Call for user2 - should capture event (different user)
    client
        .get_feature_flag("test-flag", "user2", None, None, None)
        .await
        .ok();

    flags_mock.assert_hits(2);
    capture_mock.assert_hits(2); // 2 events captured for different users
}

#[tokio::test]
async fn test_feature_flag_event_send_false() {
    // Test that send_feature_flag_events=false disables event capture
    let server = MockServer::start();

    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({
            "flags": {
                "test-flag": {
                    "key": "test-flag",
                    "enabled": true,
                    "variant": null
                }
            }
        }));
    });

    let capture_mock = server.mock(|when, then| {
        when.method(POST).path("/capture/");
        then.status(200);
    });

    // Create client with send_feature_flag_events disabled
    let options = posthog_rs::ClientOptionsBuilder::default()
        .api_key("test_api_key".to_string())
        .host(server.base_url())
        .send_feature_flag_events(false)
        .build()
        .unwrap();

    let client = posthog_rs::client(options).await;

    let result = client
        .get_feature_flag("test-flag", "test-user", None, None, None)
        .await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(FlagValue::Boolean(true)));
    flags_mock.assert();
    capture_mock.assert_hits(0); // No event captured when disabled
}

#[tokio::test]
async fn test_feature_flag_event_with_variant() {
    // Test that multivariate flag variant is captured in event
    let server = MockServer::start();

    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({
            "flags": {
                "variant-flag": {
                    "key": "variant-flag",
                    "enabled": true,
                    "variant": "control"
                }
            }
        }));
    });

    let capture_mock = server.mock(|when, then| {
        when.method(POST).path("/i/v0/e/");
        then.status(200);
    });

    let client = create_test_client(server.base_url()).await;

    let result = client
        .get_feature_flag("variant-flag", "test-user", None, None, None)
        .await;

    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        Some(FlagValue::String("control".to_string()))
    );
    flags_mock.assert();
    capture_mock.assert();
}

#[tokio::test]
async fn test_is_feature_enabled_captures_event() {
    // Test that is_feature_enabled also captures events
    let server = MockServer::start();

    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({
            "flags": {
                "enabled-flag": {
                    "key": "enabled-flag",
                    "enabled": true,
                    "variant": null
                }
            }
        }));
    });

    let capture_mock = server.mock(|when, then| {
        when.method(POST).path("/i/v0/e/").json_body_partial(
            json!({
                "event": "$feature_flag_called"
            })
            .to_string(),
        );
        then.status(200);
    });

    let client = create_test_client(server.base_url()).await;

    let result = client
        .is_feature_enabled("enabled-flag", "test-user", None, None, None)
        .await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), true);
    flags_mock.assert();
    capture_mock.assert();
}

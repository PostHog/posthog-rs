use httpmock::prelude::*;
#[cfg(feature = "async-client")]
use posthog_rs::{AsyncFlagPoller, LocalEvaluationConfig};
use posthog_rs::{
    ClientOptionsBuilder, FeatureFlag, FeatureFlagCondition, FeatureFlagFilters, FlagCache,
    FlagValue, LocalEvaluationResponse, LocalEvaluator, Property,
};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;

#[test]
fn test_local_evaluation_basic() {
    // Create a cache and evaluator
    let cache = FlagCache::new();
    let evaluator = LocalEvaluator::new(cache.clone());

    // Create a simple flag
    let flag = FeatureFlag {
        key: "test-flag".to_string(),
        active: true,
        filters: FeatureFlagFilters {
            groups: vec![FeatureFlagCondition {
                properties: vec![],
                rollout_percentage: Some(100.0),
                variant: None,
            }],
            multivariate: None,
            payloads: HashMap::new(),
        },
    };

    // Update cache with the flag
    let response = LocalEvaluationResponse {
        flags: vec![flag],
        group_type_mapping: HashMap::new(),
        cohorts: HashMap::new(),
    };
    cache.update(response);

    // Test evaluation
    let properties = HashMap::new();
    let result = evaluator.evaluate_flag("test-flag", "user-123", &properties);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(FlagValue::Boolean(true)));
}

#[test]
fn test_local_evaluation_with_properties() {
    let cache = FlagCache::new();
    let evaluator = LocalEvaluator::new(cache.clone());

    // Create a flag with property conditions
    let flag = FeatureFlag {
        key: "premium-feature".to_string(),
        active: true,
        filters: FeatureFlagFilters {
            groups: vec![FeatureFlagCondition {
                properties: vec![Property {
                    key: "plan".to_string(),
                    value: json!("premium"),
                    operator: "exact".to_string(),
                    property_type: None,
                }],
                rollout_percentage: Some(100.0),
                variant: None,
            }],
            multivariate: None,
            payloads: HashMap::new(),
        },
    };

    // Update cache
    let response = LocalEvaluationResponse {
        flags: vec![flag],
        group_type_mapping: HashMap::new(),
        cohorts: HashMap::new(),
    };
    cache.update(response);

    // Test with matching properties
    let mut properties = HashMap::new();
    properties.insert("plan".to_string(), json!("premium"));

    let result = evaluator.evaluate_flag("premium-feature", "user-123", &properties);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(FlagValue::Boolean(true)));

    // Test with non-matching properties
    let mut properties = HashMap::new();
    properties.insert("plan".to_string(), json!("free"));

    let result = evaluator.evaluate_flag("premium-feature", "user-456", &properties);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(FlagValue::Boolean(false)));
}

#[test]
fn test_local_evaluation_missing_flag() {
    let cache = FlagCache::new();
    let evaluator = LocalEvaluator::new(cache);

    let properties = HashMap::new();
    let result = evaluator.evaluate_flag("non-existent", "user-123", &properties);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), None);
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn test_local_evaluation_with_mock_server() {
    let server = MockServer::start();

    // Mock the local evaluation endpoint
    let mock_flags = json!({
        "flags": [
            {
                "key": "feature-a",
                "active": true,
                "filters": {
                    "groups": [
                        {
                            "properties": [],
                            "rollout_percentage": 50.0,
                            "variant": null
                        }
                    ],
                    "multivariate": null,
                    "payloads": {}
                }
            },
            {
                "key": "feature-b",
                "active": true,
                "filters": {
                    "groups": [
                        {
                            "properties": [
                                {
                                    "key": "email",
                                    "value": "@company.com",
                                    "operator": "icontains"
                                }
                            ],
                            "rollout_percentage": 100.0,
                            "variant": null
                        }
                    ],
                    "multivariate": null,
                    "payloads": {}
                }
            }
        ],
        "group_type_mapping": {},
        "cohorts": {}
    });

    let eval_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/api/feature_flag/local_evaluation/")
            .header("Authorization", "Bearer test_personal_key")
            .header("X-PostHog-Project-Api-Key", "test_project_key")
            .query_param("send_cohorts", "");
        then.status(200).json_body(mock_flags);
    });

    // Create client with local evaluation enabled
    let options = ClientOptionsBuilder::default()
        .host(server.base_url())
        .api_key("test_project_key".to_string())
        .personal_api_key("test_personal_key".to_string())
        .enable_local_evaluation(true)
        .poll_interval_seconds(60)
        .build()
        .unwrap();

    let client = posthog_rs::client(options).await;

    // Give it a moment to load initial flags
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Test local evaluation
    let mut properties = HashMap::new();
    properties.insert("email".to_string(), json!("test@company.com"));

    let result = client
        .get_feature_flag("feature-b", "user-123", None, Some(properties), None)
        .await;

    assert!(result.unwrap() == Some(FlagValue::Boolean(true)));

    eval_mock.assert();
}

#[test]
fn test_cache_operations() {
    let cache = FlagCache::new();

    // Create multiple flags
    let flags = vec![
        FeatureFlag {
            key: "flag1".to_string(),
            active: true,
            filters: FeatureFlagFilters {
                groups: vec![],
                multivariate: None,
                payloads: HashMap::new(),
            },
        },
        FeatureFlag {
            key: "flag2".to_string(),
            active: true,
            filters: FeatureFlagFilters {
                groups: vec![],
                multivariate: None,
                payloads: HashMap::new(),
            },
        },
    ];

    let response = LocalEvaluationResponse {
        flags: flags.clone(),
        group_type_mapping: HashMap::new(),
        cohorts: HashMap::new(),
    };

    cache.update(response);

    // Test get_flag
    assert!(cache.get_flag("flag1").is_some());
    assert!(cache.get_flag("flag2").is_some());
    assert!(cache.get_flag("flag3").is_none());

    // Test get_all_flags
    let all_flags = cache.get_all_flags();
    assert_eq!(all_flags.len(), 2);

    // Test clear
    cache.clear();
    assert!(cache.get_flag("flag1").is_none());
    assert_eq!(cache.get_all_flags().len(), 0);
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn test_etag_sent_on_second_poll() {
    let server = MockServer::start();

    let mock_flags = json!({
        "flags": [{
            "key": "test-flag",
            "active": true,
            "filters": {
                "groups": [{"properties": [], "rollout_percentage": 100.0, "variant": null}],
                "multivariate": null,
                "payloads": {}
            }
        }],
        "group_type_mapping": {},
        "cohorts": {}
    });

    // Mock for requests WITH If-None-Match header (subsequent polls) -> 304
    // Registered FIRST but uses matches() to only match when header is present
    let etag_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/api/feature_flag/local_evaluation/")
            .query_param("send_cohorts", "")
            .matches(|req| {
                // Match only if If-None-Match header exists
                req.headers.as_ref().is_some_and(|headers| {
                    headers
                        .iter()
                        .any(|(name, _)| name.to_lowercase() == "if-none-match")
                })
            });
        then.status(304);
    });

    // Mock for requests WITHOUT If-None-Match (initial load + first poll) -> 200 with ETag
    // Registered SECOND - will be tried first but etag_mock's matches() will fail if no header
    let no_etag_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/api/feature_flag/local_evaluation/")
            .query_param("send_cohorts", "");
        then.status(200)
            .header("ETag", "\"abc123\"")
            .json_body(mock_flags.clone());
    });

    let cache = FlagCache::new();
    let config = LocalEvaluationConfig {
        personal_api_key: "test_personal_key".to_string(),
        project_api_key: "test_project_key".to_string(),
        api_host: server.base_url(),
        poll_interval: Duration::from_millis(100),
        request_timeout: Duration::from_secs(5),
    };

    let mut poller = AsyncFlagPoller::new(config, cache.clone());
    poller.start().await;

    // Wait for:
    // - Initial load (immediate) -> gets 200 with ETag (load_flags doesn't set last_etag)
    // - First poll tick (after 100ms) -> gets 200, sets last_etag in the polling loop
    // - Second poll tick (after 200ms) -> sends If-None-Match, gets 304
    tokio::time::sleep(Duration::from_millis(350)).await;

    poller.stop().await;

    // Verify requests without If-None-Match were made (initial load + first poll)
    assert!(
        no_etag_mock.hits() >= 2,
        "Should have at least 2 requests without If-None-Match (initial + first poll), got {}",
        no_etag_mock.hits()
    );

    // Verify at least one request WITH If-None-Match was made (second+ poll)
    assert!(
        etag_mock.hits() >= 1,
        "Should have at least 1 request with If-None-Match header, got {}",
        etag_mock.hits()
    );

    // Verify cache has the flag (preserved after 304)
    assert!(
        cache.get_flag("test-flag").is_some(),
        "Flag should be in cache"
    );
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn test_304_preserves_cache() {
    let server = MockServer::start();

    let mock_flags = json!({
        "flags": [{
            "key": "preserved-flag",
            "active": true,
            "filters": {
                "groups": [{"properties": [], "rollout_percentage": 100.0, "variant": null}],
                "multivariate": null,
                "payloads": {}
            }
        }],
        "group_type_mapping": {},
        "cohorts": {}
    });

    // Mock for requests WITH If-None-Match -> returns 304
    // Registered FIRST but uses matches() to only match when header is present
    let etag_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/api/feature_flag/local_evaluation/")
            .query_param("send_cohorts", "")
            .matches(|req| {
                // Match only if If-None-Match header exists
                req.headers.as_ref().is_some_and(|headers| {
                    headers
                        .iter()
                        .any(|(name, _)| name.to_lowercase() == "if-none-match")
                })
            });
        then.status(304);
    });

    // Mock for requests WITHOUT If-None-Match -> returns 200 with ETag
    // Registered SECOND - will be tried first but etag_mock's matches() will fail if no header
    server.mock(|when, then| {
        when.method(GET)
            .path("/api/feature_flag/local_evaluation/")
            .query_param("send_cohorts", "");
        then.status(200)
            .header("ETag", "\"v1\"")
            .json_body(mock_flags);
    });

    let cache = FlagCache::new();
    let config = LocalEvaluationConfig {
        personal_api_key: "test_personal_key".to_string(),
        project_api_key: "test_project_key".to_string(),
        api_host: server.base_url(),
        poll_interval: Duration::from_millis(100),
        request_timeout: Duration::from_secs(5),
    };

    let mut poller = AsyncFlagPoller::new(config, cache.clone());
    poller.start().await;

    // Wait for initial load
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Verify flag is in cache after initial load
    assert!(
        cache.get_flag("preserved-flag").is_some(),
        "Flag should be loaded initially"
    );

    // Wait for multiple poll cycles (enough for at least one 304 response)
    tokio::time::sleep(Duration::from_millis(350)).await;

    poller.stop().await;

    // Verify 304 mock was hit (proves If-None-Match was sent and 304 was received)
    assert!(
        etag_mock.hits() >= 1,
        "Should have received at least one 304 response, got {} hits",
        etag_mock.hits()
    );

    // Flag should still be present after 304 response (cache preserved)
    assert!(
        cache.get_flag("preserved-flag").is_some(),
        "Flag should remain in cache after 304 response"
    );
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn test_no_etag_from_server() {
    let server = MockServer::start();

    let mock_flags = json!({
        "flags": [{
            "key": "no-etag-flag",
            "active": true,
            "filters": {
                "groups": [{"properties": [], "rollout_percentage": 100.0, "variant": null}],
                "multivariate": null,
                "payloads": {}
            }
        }],
        "group_type_mapping": {},
        "cohorts": {}
    });

    // Server returns 200 without ETag header
    let mock = server.mock(|when, then| {
        when.method(GET).path("/api/feature_flag/local_evaluation/");
        then.status(200).json_body(mock_flags);
    });

    let cache = FlagCache::new();
    let config = LocalEvaluationConfig {
        personal_api_key: "test_personal_key".to_string(),
        project_api_key: "test_project_key".to_string(),
        api_host: server.base_url(),
        poll_interval: Duration::from_millis(50),
        request_timeout: Duration::from_secs(5),
    };

    let mut poller = AsyncFlagPoller::new(config, cache.clone());
    poller.start().await;

    // Wait for initial load + a couple poll cycles
    tokio::time::sleep(Duration::from_millis(150)).await;

    poller.stop().await;

    // Should have made multiple requests (initial + polls), all without If-None-Match
    assert!(
        mock.hits() >= 2,
        "Should have made multiple requests without ETag"
    );

    // Cache should have the flag
    assert!(
        cache.get_flag("no-etag-flag").is_some(),
        "Flag should be in cache"
    );
}

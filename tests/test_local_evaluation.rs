use posthog_rs::{
    FeatureFlag, FeatureFlagCondition, FeatureFlagFilters, FlagCache, FlagValue,
    LocalEvaluationResponse, LocalEvaluator, Property,
};
use serde_json::json;
use std::collections::HashMap;

#[cfg(feature = "async-client")]
use httpmock::prelude::*;
#[cfg(feature = "async-client")]
use posthog_rs::ClientOptionsBuilder;
#[cfg(feature = "async-client")]
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
            .query_param("token", "test_project_key")
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

    assert!(result.is_ok());
    // The actual result depends on whether the mock was hit and processed

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

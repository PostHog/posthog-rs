mod common;

use common::default_user_agent;
use httpmock::prelude::*;
#[cfg(feature = "async-client")]
use posthog::{AsyncFlagPoller, ClientOptionsBuilder, EvaluateFlagsOptions};
use posthog::{
    FeatureFlag, FeatureFlagCondition, FeatureFlagFilters, FlagCache, FlagPoller, FlagValue,
    LocalEvaluationConfig, LocalEvaluationResponse, LocalEvaluator, Property,
};
use reqwest::header::USER_AGENT;
use serde_json::json;
use std::collections::HashMap;
use std::time::{Duration, Instant};

fn wait_for_calls(mock: &httpmock::Mock<'_>, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while mock.calls() < count {
        assert!(
            Instant::now() < deadline,
            "poller did not reach {} requests",
            count
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(feature = "async-client")]
async fn wait_for_calls_async(mock: &httpmock::Mock<'_>, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while mock.calls_async().await < count {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("poller did not reach expected request count");
}

#[test]
fn test_local_evaluation_basic() {
    // Create a cache and evaluator
    let cache = FlagCache::new();
    let evaluator = LocalEvaluator::new(cache.clone());

    // Create a simple flag
    let flag = FeatureFlag {
        key: "test-flag".to_string(),
        active: true,
        has_experiment: None,
        filters: FeatureFlagFilters {
            groups: vec![FeatureFlagCondition {
                properties: vec![],
                rollout_percentage: Some(100.0),
                variant: None,
                aggregation_group_type_index: None,
            }],
            multivariate: None,
            payloads: HashMap::new(),
            aggregation_group_type_index: None,
            early_exit: false,
        },
    };

    // Update cache with the flag
    let response = LocalEvaluationResponse::new(vec![flag]);
    cache.update(response);

    // Test evaluation
    let properties = HashMap::new();
    let result = evaluator.evaluate_flag(
        "test-flag",
        "user-123",
        &properties,
        &HashMap::new(),
        &HashMap::new(),
    );

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(FlagValue::Boolean(true)));
}

/// Regression test: a non-empty `cohorts` payload must deserialize (the real
/// API maps each cohort ID straight to its property group, with no `id`/`name`
/// fields) and a flag targeting that cohort must evaluate against it. Before the
/// fix, `cohorts` failed to deserialize and took down the entire
/// `LocalEvaluationResponse` — every flag, not just the cohort-targeted ones.
#[test]
fn test_local_evaluation_with_cohorts_payload() {
    // Shape mirrors `/flags/definitions/?send_cohorts`: cohort ID -> property
    // group, nested groups and OR included.
    let payload = json!({
        "flags": [
            {
                "key": "cohort-flag",
                "active": true,
                "filters": {
                    "groups": [
                        {
                            "properties": [
                                {
                                    "key": "id",
                                    "value": 42,
                                    "type": "cohort"
                                }
                            ],
                            "rollout_percentage": 100.0
                        }
                    ]
                }
            }
        ],
        "group_type_mapping": {},
        "cohorts": {
            "42": {
                "type": "AND",
                "values": [
                    {
                        "type": "OR",
                        "values": [
                            {"key": "country", "value": "US", "operator": "exact", "type": "person"},
                            {"key": "country", "value": "CA", "operator": "exact", "type": "person"}
                        ]
                    }
                ]
            }
        }
    });

    // The whole response must parse — this is the exact path that used to fail.
    let response: LocalEvaluationResponse =
        serde_json::from_value(payload).expect("cohorts payload should deserialize");
    assert_eq!(response.flags.len(), 1);
    assert_eq!(response.cohorts.len(), 1);

    let cache = FlagCache::new();
    let evaluator = LocalEvaluator::new(cache.clone());
    cache.update(response);

    // Member of the cohort (country in {US, CA}).
    let mut properties = HashMap::new();
    properties.insert("country".to_string(), json!("CA"));
    let result = evaluator.evaluate_flag(
        "cohort-flag",
        "user-123",
        &properties,
        &HashMap::new(),
        &HashMap::new(),
    );
    assert_eq!(result.unwrap(), Some(FlagValue::Boolean(true)));

    // Not a member — the OR group excludes UK.
    let mut properties = HashMap::new();
    properties.insert("country".to_string(), json!("UK"));
    let result = evaluator.evaluate_flag(
        "cohort-flag",
        "user-123",
        &properties,
        &HashMap::new(),
        &HashMap::new(),
    );
    assert_eq!(result.unwrap(), Some(FlagValue::Boolean(false)));
}

fn evaluate(
    evaluator: &LocalEvaluator,
    key: &str,
    properties: &HashMap<String, serde_json::Value>,
) -> Result<Option<FlagValue>, posthog::InconclusiveMatchError> {
    evaluator.evaluate_flag(
        key,
        "user-123",
        properties,
        &HashMap::new(),
        &HashMap::new(),
    )
}

/// The payload from the report: one flag with a value-less `is_not_set` filter
/// next to a healthy flag. Both load.
#[test]
fn test_valueless_is_not_set_filter_does_not_fail_definitions() {
    let response: LocalEvaluationResponse = serde_json::from_str(
        r#"{
          "flags": [
            {
              "key": "healthy-flag",
              "active": true,
              "filters": { "groups": [ { "properties": [], "rollout_percentage": 100 } ] }
            },
            {
              "key": "flag-with-valueless-is-not-set",
              "active": true,
              "filters": {
                "groups": [
                  {
                    "properties": [ { "key": "plan", "type": "person", "operator": "is_not_set" } ],
                    "rollout_percentage": 100
                  }
                ]
              }
            }
          ],
          "group_type_mapping": {},
          "cohorts": {}
        }"#,
    )
    .expect("a value-less is_not_set filter should deserialize");
    assert_eq!(response.flags.len(), 2);

    let cache = FlagCache::new();
    let evaluator = LocalEvaluator::new(cache.clone());
    cache.update(response);

    let with_plan = HashMap::from([("plan".to_string(), json!("pro"))]);
    assert_eq!(
        evaluate(&evaluator, "healthy-flag", &with_plan).unwrap(),
        Some(FlagValue::Boolean(true))
    );
    assert_eq!(
        evaluate(&evaluator, "flag-with-valueless-is-not-set", &with_plan).unwrap(),
        Some(FlagValue::Boolean(false))
    );
}

/// Regression test: the API accepts `is_set` / `is_not_set` filters without a
/// `value`, on flags and on cohort leaves alike. Before the fix one such filter
/// failed to deserialize and took down the entire `LocalEvaluationResponse`,
/// so no flag in the project could be evaluated locally.
#[test]
fn test_local_evaluation_with_valueless_presence_operators() {
    let presence_flag = |key: &str, operator: &str| {
        json!({
            "key": key,
            "active": true,
            "filters": {
                "groups": [
                    {
                        "properties": [{"key": "plan", "type": "person", "operator": operator}],
                        "rollout_percentage": 100.0
                    }
                ]
            }
        })
    };
    let payload = json!({
        "flags": [
            {
                "key": "plain-flag",
                "active": true,
                "filters": {"groups": [{"properties": [], "rollout_percentage": 100.0}]}
            },
            presence_flag("plan-set", "is_set"),
            presence_flag("plan-not-set", "is_not_set"),
            {
                "key": "cohort-flag",
                "active": true,
                "filters": {
                    "groups": [
                        {
                            "properties": [{"key": "id", "value": 7, "type": "cohort"}],
                            "rollout_percentage": 100.0
                        }
                    ]
                }
            }
        ],
        "group_type_mapping": {},
        "cohorts": {
            "7": {
                "type": "AND",
                "values": [{"key": "plan", "type": "person", "operator": "is_set"}]
            }
        }
    });

    let response: LocalEvaluationResponse =
        serde_json::from_value(payload).expect("value-less presence filters should deserialize");
    assert_eq!(response.flags.len(), 4);

    let cache = FlagCache::new();
    let evaluator = LocalEvaluator::new(cache.clone());
    cache.update(response);

    let with_plan = HashMap::from([("plan".to_string(), json!("pro"))]);
    let without_plan = HashMap::new();

    for (key, expected) in [
        ("plain-flag", true),
        ("plan-set", true),
        ("plan-not-set", false),
        ("cohort-flag", true),
    ] {
        assert_eq!(
            evaluate(&evaluator, key, &with_plan).unwrap(),
            Some(FlagValue::Boolean(expected)),
            "{}",
            key
        );
    }

    // A property missing from the caller's context stays inconclusive.
    for key in ["plan-set", "plan-not-set", "cohort-flag"] {
        assert!(evaluate(&evaluator, key, &without_plan).is_err(), "{}", key);
    }
}

fn poller_for(server: &MockServer, cache: FlagCache) -> FlagPoller {
    let config = LocalEvaluationConfig {
        secret_key: "test_personal_key".to_string(),
        project_api_key: "test_project_key".to_string(),
        api_host: server.base_url(),
        poll_interval: Duration::from_secs(60),
        request_timeout: Duration::from_secs(5),
    };
    FlagPoller::new(config, cache)
}

/// A definition this SDK cannot read is skipped on its own when definitions
/// load: the other flags in the response still load, and the skipped flag is
/// absent from the cache, like any other flag local evaluation cannot resolve.
#[test]
fn test_unreadable_flag_definition_does_not_drop_other_flags() {
    let server = MockServer::start();
    let definitions = server.mock(|when, then| {
        when.method(GET).path("/flags/definitions/");
        then.status(200)
            .header("content-type", "application/json")
            .body(
                r#"{
                    "flags": [
                        {
                            "key": "exact-without-value",
                            "active": true,
                            "filters": {"groups": [{
                                "properties": [{"key": "plan", "type": "person", "operator": "exact"}],
                                "rollout_percentage": 100
                            }]}
                        },
                        {"key": "active-is-not-a-boolean", "active": "yes"},
                        {"key": "duplicate-key", "active": false, "active": true},
                        {
                            "key": "plain-flag",
                            "active": true,
                            "filters": {"groups": [{"properties": [], "rollout_percentage": 100}]}
                        }
                    ],
                    "group_type_mapping": {},
                    "cohorts": {}
                }"#,
            );
    });

    let cache = FlagCache::new();
    poller_for(&server, cache.clone())
        .load_flags()
        .expect("unreadable flags should not fail the load");
    definitions.assert();

    let evaluator = LocalEvaluator::new(cache);
    let properties = HashMap::from([("plan".to_string(), json!("pro"))]);
    assert_eq!(
        evaluate(&evaluator, "plain-flag", &properties).unwrap(),
        Some(FlagValue::Boolean(true))
    );
    for skipped in [
        "exact-without-value",
        "active-is-not-a-boolean",
        "duplicate-key",
    ] {
        assert_eq!(
            evaluate(&evaluator, skipped, &properties).unwrap(),
            None,
            "{}",
            skipped
        );
    }
}

/// Skipping is confined to flag definitions that fail on their own. A response
/// that is unreadable for any other reason is still rejected whole, and the
/// definitions already cached stay in place.
#[test]
fn test_definitions_unreadable_outside_flags_are_still_rejected() {
    let unreadable_flag = r#"{"key":"active-is-not-a-boolean","active":"yes"}"#;
    let bodies = [
        // A field outside `flags` has the wrong type.
        [
            r#"{"flags":["#,
            unreadable_flag,
            r#"],"group_type_mapping":"not-a-map"}"#,
        ]
        .concat(),
        // A key outside `flags` is repeated.
        [
            r#"{"flags":["#,
            unreadable_flag,
            r#"],"cohorts":{},"cohorts":{}}"#,
        ]
        .concat(),
    ];

    for body in bodies {
        let server = MockServer::start();
        let definitions = server.mock(|when, then| {
            when.method(GET).path("/flags/definitions/");
            then.status(200)
                .header("content-type", "application/json")
                .body(&body);
        });

        let cache = FlagCache::new();
        cache.update(
            serde_json::from_value(json!({
                "flags": [{"key": "cached-flag", "active": true}]
            }))
            .unwrap(),
        );

        let result = poller_for(&server, cache.clone()).load_flags();
        definitions.assert();
        assert!(
            matches!(result, Err(posthog::Error::Serialization(_))),
            "{}",
            body
        );
        assert!(cache.get_flag("cached-flag").is_some(), "{}", body);
    }
}

/// A filter without a value, on an operator that compares against one, makes
/// its flag unreadable. That flag alone is deferred: the client has no local
/// result for it and asks `/flags`, while the other flag still resolves locally.
#[cfg(feature = "async-client")]
#[tokio::test]
async fn test_value_requiring_operator_without_value_defers_only_that_flag() {
    let server = MockServer::start();
    let definitions = server.mock(|when, then| {
        when.method(GET).path("/flags/definitions/");
        then.status(200)
            .header("content-type", "application/json")
            .body(
                r#"{
                    "flags": [
                        {
                            "key": "exact-without-value",
                            "active": true,
                            "filters": {"groups": [{
                                "properties": [{"key": "plan", "type": "person", "operator": "exact"}],
                                "rollout_percentage": 100
                            }]}
                        },
                        {
                            "key": "plain-flag",
                            "active": true,
                            "filters": {"groups": [{"properties": [], "rollout_percentage": 100}]}
                        }
                    ],
                    "group_type_mapping": {},
                    "cohorts": {}
                }"#,
            );
    });
    let remote = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({
            "flags": {
                "exact-without-value": {
                    "key": "exact-without-value",
                    "enabled": true,
                    "variant": null,
                    "reason": {
                        "code": "condition_match",
                        "description": null,
                        "condition_index": 0
                    },
                    "metadata": {"id": 1, "version": 1, "description": null, "payload": null}
                }
            }
        }));
    });

    let options = ClientOptionsBuilder::default()
        .host(server.base_url())
        .api_key("test_project_key".to_string())
        .secret_key("test_personal_key".to_string())
        .enable_local_evaluation(true)
        .poll_interval_seconds(60)
        .build()
        .unwrap();
    let client = posthog::client(options).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    definitions.assert();

    let evaluate = |key: &str, only_evaluate_locally: bool| {
        let mut options = EvaluateFlagsOptions::default();
        options.flag_keys = Some(vec![key.to_string()]);
        options.person_properties = Some(HashMap::from([("plan".to_string(), json!("pro"))]));
        options.only_evaluate_locally = only_evaluate_locally;
        client.evaluate_flags("user-123", options)
    };

    // The readable flag resolves locally, with no request to `/flags`.
    let snapshot = evaluate("plain-flag", false).await.unwrap();
    assert_eq!(
        snapshot.get_flag("plain-flag"),
        Some(FlagValue::Boolean(true))
    );
    remote.assert_calls(0);

    // The other one has no local result ...
    let snapshot = evaluate("exact-without-value", true).await.unwrap();
    assert_eq!(snapshot.get_flag("exact-without-value"), None);
    remote.assert_calls(0);

    // ... and `/flags` decides it when remote evaluation is allowed.
    let snapshot = evaluate("exact-without-value", false).await.unwrap();
    assert_eq!(
        snapshot.get_flag("exact-without-value"),
        Some(FlagValue::Boolean(true))
    );
    remote.assert_calls(1);
}

/// A cyclic cohort definition must leave local evaluation inconclusive so the
/// flag falls back to `/flags`. Before the cycle guard this recursed until the
/// stack was exhausted, which aborts the process rather than panicking, taking
/// down the caller's server.
#[test]
fn test_cyclic_cohort_definition_does_not_exhaust_the_stack() {
    let payload = json!({
        "flags": [
            {
                "key": "cohort-flag",
                "active": true,
                "filters": {
                    "groups": [
                        {
                            "properties": [
                                {"key": "id", "value": 1, "type": "cohort"}
                            ],
                            "rollout_percentage": 100.0
                        }
                    ]
                }
            }
        ],
        "group_type_mapping": {},
        "cohorts": {
            "1": {
                "type": "AND",
                "values": [{"type": "cohort", "value": 2}]
            },
            "2": {
                "type": "AND",
                "values": [{"type": "cohort", "value": 1}]
            }
        }
    });

    let response: LocalEvaluationResponse =
        serde_json::from_value(payload).expect("cohorts payload should deserialize");
    let cache = FlagCache::new();
    let evaluator = LocalEvaluator::new(cache.clone());
    cache.update(response);

    let result = evaluator.evaluate_flag(
        "cohort-flag",
        "user-123",
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    );

    assert!(
        result.is_err(),
        "a cohort cycle must be inconclusive so the flag falls back to /flags"
    );
}

/// Nested property groups contribute to stack depth as well as cohort links.
/// This payload stays within serde_json's per-value nesting limit but combines
/// both forms deeply enough to overflow a typical worker stack without a shared
/// resolution-depth bound.
#[test]
fn test_combined_cohort_and_property_group_depth_is_inconclusive() {
    let cohort_count = 100;
    let groups_per_cohort = 50;
    let mut cohorts = serde_json::Map::new();

    for cohort_index in 0..cohort_count {
        let mut properties = if cohort_index == cohort_count - 1 {
            json!({"key": "country", "value": "US", "operator": "exact", "type": "person"})
        } else {
            json!({"type": "cohort", "value": (cohort_index + 1).to_string()})
        };

        for _ in 0..groups_per_cohort {
            properties = json!({"type": "AND", "values": [properties]});
        }

        cohorts.insert(cohort_index.to_string(), properties);
    }

    let payload = json!({
        "flags": [
            {
                "key": "cohort-flag",
                "active": true,
                "filters": {
                    "groups": [
                        {
                            "properties": [
                                {"key": "id", "value": "0", "type": "cohort"}
                            ],
                            "rollout_percentage": 100.0
                        }
                    ]
                }
            }
        ],
        "group_type_mapping": {},
        "cohorts": serde_json::Value::Object(cohorts)
    });
    let serialized = serde_json::to_string(&payload).expect("payload should serialize");
    let response: LocalEvaluationResponse =
        serde_json::from_str(&serialized).expect("payload should stay within the JSON depth limit");

    let cache = FlagCache::new();
    let evaluator = LocalEvaluator::new(cache.clone());
    cache.update(response);

    let result = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            evaluator.evaluate_flag(
                "cohort-flag",
                "user-123",
                &HashMap::from([("country".to_string(), json!("US"))]),
                &HashMap::new(),
                &HashMap::new(),
            )
        })
        .expect("evaluation thread should start")
        .join()
        .expect("evaluation thread should not abort or panic");

    assert!(
        result.is_err(),
        "combined cohort and property-group depth must be inconclusive"
    );
}

#[test]
fn test_local_evaluation_with_properties() {
    let cache = FlagCache::new();
    let evaluator = LocalEvaluator::new(cache.clone());

    // Create a flag with property conditions
    let flag = FeatureFlag {
        key: "premium-feature".to_string(),
        active: true,
        has_experiment: None,
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
                aggregation_group_type_index: None,
            }],
            multivariate: None,
            payloads: HashMap::new(),
            aggregation_group_type_index: None,
            early_exit: false,
        },
    };

    // Update cache
    let response = LocalEvaluationResponse::new(vec![flag]);
    cache.update(response);

    // Test with matching properties
    let mut properties = HashMap::new();
    properties.insert("plan".to_string(), json!("premium"));

    let result = evaluator.evaluate_flag(
        "premium-feature",
        "user-123",
        &properties,
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(FlagValue::Boolean(true)));

    // Test with non-matching properties
    let mut properties = HashMap::new();
    properties.insert("plan".to_string(), json!("free"));

    let result = evaluator.evaluate_flag(
        "premium-feature",
        "user-456",
        &properties,
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(FlagValue::Boolean(false)));
}

#[test]
fn test_local_evaluation_missing_flag() {
    let cache = FlagCache::new();
    let evaluator = LocalEvaluator::new(cache);

    let properties = HashMap::new();
    let result = evaluator.evaluate_flag(
        "non-existent",
        "user-123",
        &properties,
        &HashMap::new(),
        &HashMap::new(),
    );

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
            .path("/flags/definitions/")
            .header("Authorization", "Bearer test_personal_key")
            .header("X-PostHog-Project-Api-Key", "test_project_key")
            .query_param("send_cohorts", "");
        then.status(200).json_body(mock_flags);
    });

    // Create client with local evaluation enabled
    let options = ClientOptionsBuilder::default()
        .host(server.base_url())
        .api_key("test_project_key".to_string())
        .secret_key("test_personal_key".to_string())
        .enable_local_evaluation(true)
        .poll_interval_seconds(60)
        .build()
        .unwrap();

    let client = posthog::client(options).await;

    // Give it a moment to load initial flags
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Test local evaluation
    let mut properties = HashMap::new();
    properties.insert("email".to_string(), json!("test@company.com"));

    let mut options = EvaluateFlagsOptions::default();
    options.person_properties = Some(properties);
    options.flag_keys = Some(vec!["feature-b".to_string()]);
    let flags = client.evaluate_flags("user-123", options).await.unwrap();

    assert_eq!(flags.get_flag("feature-b"), Some(FlagValue::Boolean(true)));

    eval_mock.assert();
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn test_client_adds_distinct_id_for_local_evaluation_only() {
    let server = MockServer::start();

    let mock_flags = json!({
        "flags": [
            {
                "key": "distinct-id-flag",
                "active": true,
                "filters": {
                    "groups": [
                        {
                            "properties": [
                                {
                                    "key": "distinct_id",
                                    "value": "user-123",
                                    "operator": "exact"
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
        when.method(GET).path("/flags/definitions/");
        then.status(200).json_body(mock_flags);
    });
    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({
            "featureFlags": {"distinct-id-flag": false},
            "featureFlagPayloads": {}
        }));
    });

    let options = ClientOptionsBuilder::default()
        .host(server.base_url())
        .api_key("test_project_key".to_string())
        .secret_key("test_personal_key".to_string())
        .enable_local_evaluation(true)
        .local_evaluation_only(true)
        .poll_interval_seconds(60)
        .build()
        .unwrap();

    let client = posthog::client(options).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let flags = client
        .evaluate_flags("user-123", EvaluateFlagsOptions::default())
        .await
        .unwrap();

    assert_eq!(
        flags.get_flag("distinct-id-flag"),
        Some(FlagValue::Boolean(true))
    );
    eval_mock.assert();
    flags_mock.assert_calls(0);
}

/// Payloads ride along in the definitions manifest, so a snapshot that never
/// touches `/flags` must still expose them. This is the path where they were
/// most thoroughly lost: with `flag_keys` fully covered locally there is no
/// remote round trip left to recover a payload from.
#[cfg(feature = "async-client")]
#[tokio::test]
async fn test_local_evaluation_returns_payloads_without_calling_flags() {
    let server = MockServer::start();

    // Payloads arrive JSON-encoded from `/flags/definitions/`, the same shape
    // `/flags` returns them in.
    let mock_flags = json!({
        "flags": [
            {
                "key": "bool-flag",
                "active": true,
                "filters": {
                    "groups": [
                        { "properties": [], "rollout_percentage": 100.0, "variant": null }
                    ],
                    "multivariate": null,
                    "payloads": { "true": "{\"color\": \"blue\"}" }
                }
            },
            {
                "key": "variant-flag",
                "active": true,
                "filters": {
                    "groups": [
                        { "properties": [], "rollout_percentage": 100.0, "variant": null }
                    ],
                    "multivariate": {
                        "variants": [
                            { "key": "test", "rollout_percentage": 100.0 },
                            { "key": "control", "rollout_percentage": 0.0 }
                        ]
                    },
                    "payloads": { "test": "{\"tier\": 2}", "control": "{\"tier\": 1}" }
                }
            },
            {
                "key": "no-payload-flag",
                "active": true,
                "filters": {
                    "groups": [
                        { "properties": [], "rollout_percentage": 100.0, "variant": null }
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
        when.method(GET).path("/flags/definitions/");
        then.status(200).json_body(mock_flags);
    });
    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({
            "featureFlags": {},
            "featureFlagPayloads": {}
        }));
    });

    let options = ClientOptionsBuilder::default()
        .host(server.base_url())
        .api_key("test_project_key".to_string())
        .secret_key("test_personal_key".to_string())
        .enable_local_evaluation(true)
        .poll_interval_seconds(60)
        .build()
        .unwrap();

    let client = posthog::client(options).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut options = EvaluateFlagsOptions::default();
    options.flag_keys = Some(vec![
        "bool-flag".to_string(),
        "variant-flag".to_string(),
        "no-payload-flag".to_string(),
    ]);
    let snapshot = client
        .evaluate_flags("user-123", options)
        .await
        .expect("evaluate_flags");

    assert_eq!(
        snapshot.get_flag_payload("bool-flag"),
        Some(json!({ "color": "blue" }))
    );
    assert_eq!(
        snapshot.get_flag("variant-flag"),
        Some(FlagValue::String("test".to_string()))
    );
    assert_eq!(
        snapshot.get_flag_payload("variant-flag"),
        Some(json!({ "tier": 2 }))
    );
    assert_eq!(snapshot.get_flag_payload("no-payload-flag"), None);

    eval_mock.assert();
    flags_mock.assert_calls(0);
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn test_local_evaluation_with_mock_server_sends_default_user_agent() {
    let server = MockServer::start();

    // Mock the local evaluation endpoint
    let mock_flags = json!({
        "flags": [],
        "group_type_mapping": {},
        "cohorts": {}
    });

    let eval_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/flags/definitions/")
            .header("Authorization", "Bearer test_personal_key")
            .header("X-PostHog-Project-Api-Key", "test_project_key")
            .header(USER_AGENT.to_string(), default_user_agent())
            .query_param("send_cohorts", "");
        then.status(200).json_body(mock_flags);
    });

    // Create client with local evaluation enabled
    let options = ClientOptionsBuilder::default()
        .host(server.base_url())
        .api_key("test_project_key".to_string())
        .secret_key("test_personal_key".to_string())
        .enable_local_evaluation(true)
        .poll_interval_seconds(60)
        .build()
        .unwrap();

    let _client = posthog::client(options).await;

    // Give it a moment to load initial flags
    tokio::time::sleep(Duration::from_millis(100)).await;

    eval_mock.assert();
}

#[test]
fn test_sync_local_evaluation_with_mock_server_sends_default_user_agent() {
    let server = MockServer::start();

    let mock_flags = json!({
        "flags": [],
        "group_type_mapping": {},
        "cohorts": {}
    });

    let eval_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/flags/definitions/")
            .header("Authorization", "Bearer test_personal_key")
            .header("X-PostHog-Project-Api-Key", "test_project_key")
            .header(USER_AGENT.to_string(), default_user_agent())
            .query_param("send_cohorts", "");
        then.status(200).json_body(mock_flags);
    });

    let cache = FlagCache::new();
    let config = LocalEvaluationConfig {
        secret_key: "test_personal_key".to_string(),
        project_api_key: "test_project_key".to_string(),
        api_host: server.base_url(),
        poll_interval: Duration::from_secs(60),
        request_timeout: Duration::from_secs(5),
    };

    let poller = FlagPoller::new(config, cache);
    poller.load_flags().unwrap();

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
            has_experiment: None,
            filters: FeatureFlagFilters {
                groups: vec![],
                multivariate: None,
                payloads: HashMap::new(),
                aggregation_group_type_index: None,
                early_exit: false,
            },
        },
        FeatureFlag {
            key: "flag2".to_string(),
            active: true,
            has_experiment: None,
            filters: FeatureFlagFilters {
                groups: vec![],
                multivariate: None,
                payloads: HashMap::new(),
                aggregation_group_type_index: None,
                early_exit: false,
            },
        },
    ];

    let response = LocalEvaluationResponse::new(flags.clone());

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
    // Registered FIRST but uses matches() to only match when header is present with correct value
    let etag_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/flags/definitions/")
            .query_param("send_cohorts", "")
            .is_true(|req| {
                // Match only if If-None-Match header exists with the correct value
                req.headers_vec().iter().any(|(name, value)| {
                    name.eq_ignore_ascii_case("if-none-match") && value == "\"abc123\""
                })
            });
        then.status(304);
    });

    // Mock for requests WITHOUT If-None-Match (initial load + first poll) -> 200 with ETag
    // Registered SECOND - will be tried first but etag_mock's matches() will fail if no header
    let no_etag_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/flags/definitions/")
            .query_param("send_cohorts", "");
        then.status(200)
            .header("ETag", "\"abc123\"")
            .json_body(mock_flags.clone());
    });

    let cache = FlagCache::new();
    let config = LocalEvaluationConfig {
        secret_key: "test_personal_key".to_string(),
        project_api_key: "test_project_key".to_string(),
        api_host: server.base_url(),
        poll_interval: Duration::from_millis(100),
        request_timeout: Duration::from_secs(5),
    };

    let mut poller = AsyncFlagPoller::new(config, cache.clone());
    poller.start().await;

    wait_for_calls_async(&etag_mock, 2).await;
    poller.stop().await;
    assert!(no_etag_mock.calls() >= 1);

    // Verify at least one request WITH If-None-Match was made (second+ poll)
    assert!(
        etag_mock.calls() >= 1,
        "Should have at least 1 request with If-None-Match header, got {}",
        etag_mock.calls()
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
    // Registered FIRST but uses matches() to only match when header is present with correct value
    let etag_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/flags/definitions/")
            .query_param("send_cohorts", "")
            .is_true(|req| {
                // Match only if If-None-Match header exists with the correct value
                req.headers_vec().iter().any(|(name, value)| {
                    name.eq_ignore_ascii_case("if-none-match") && value == "\"v1\""
                })
            });
        then.status(304);
    });

    // Mock for requests WITHOUT If-None-Match -> returns 200 with ETag
    // Registered SECOND - will be tried first but etag_mock's matches() will fail if no header
    server.mock(|when, then| {
        when.method(GET)
            .path("/flags/definitions/")
            .query_param("send_cohorts", "");
        then.status(200)
            .header("ETag", "\"v1\"")
            .json_body(mock_flags);
    });

    let cache = FlagCache::new();
    let config = LocalEvaluationConfig {
        secret_key: "test_personal_key".to_string(),
        project_api_key: "test_project_key".to_string(),
        api_host: server.base_url(),
        poll_interval: Duration::from_millis(100),
        request_timeout: Duration::from_secs(5),
    };

    let mut poller = AsyncFlagPoller::new(config, cache.clone());
    poller.start().await;

    // Verify flag is in cache after initial load
    assert!(
        cache.get_flag("preserved-flag").is_some(),
        "Flag should be loaded initially"
    );

    wait_for_calls_async(&etag_mock, 2).await;

    poller.stop().await;

    // Verify 304 mock was hit (proves If-None-Match was sent and 304 was received)
    assert!(
        etag_mock.calls() >= 1,
        "Should have received at least one 304 response, got {} hits",
        etag_mock.calls()
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

    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/flags/definitions/")
            .header_missing("if-none-match");
        then.status(200).json_body(mock_flags);
    });
    let unexpected_etag = server.mock(|when, then| {
        when.method(GET)
            .path("/flags/definitions/")
            .header_exists("if-none-match");
        then.status(400);
    });

    let cache = FlagCache::new();
    let config = LocalEvaluationConfig {
        secret_key: "test_personal_key".to_string(),
        project_api_key: "test_project_key".to_string(),
        api_host: server.base_url(),
        poll_interval: Duration::from_millis(50),
        request_timeout: Duration::from_secs(5),
    };

    let mut poller = AsyncFlagPoller::new(config, cache.clone());
    poller.start().await;

    wait_for_calls_async(&mock, 3).await;

    poller.stop().await;
    unexpected_etag.assert_calls(0);

    // Should have made multiple requests (initial + polls), all without If-None-Match
    assert!(
        mock.calls() >= 2,
        "Should have made multiple requests without ETag"
    );

    // Cache should have the flag
    assert!(
        cache.get_flag("no-etag-flag").is_some(),
        "Flag should be in cache"
    );
}

// Sync FlagPoller tests

#[test]
fn test_sync_etag_sent_on_second_poll() {
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
    let etag_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/flags/definitions/")
            .query_param("send_cohorts", "")
            .is_true(|req| {
                // Match only if If-None-Match header exists with the correct value
                req.headers_vec().iter().any(|(name, value)| {
                    name.eq_ignore_ascii_case("if-none-match") && value == "\"sync-abc123\""
                })
            });
        then.status(304);
    });

    // Mock for requests WITHOUT If-None-Match -> 200 with ETag
    let no_etag_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/flags/definitions/")
            .query_param("send_cohorts", "");
        then.status(200)
            .header("ETag", "\"sync-abc123\"")
            .json_body(mock_flags.clone());
    });

    let cache = FlagCache::new();
    let config = LocalEvaluationConfig {
        secret_key: "test_personal_key".to_string(),
        project_api_key: "test_project_key".to_string(),
        api_host: server.base_url(),
        poll_interval: Duration::from_millis(100),
        request_timeout: Duration::from_secs(5),
    };

    let mut poller = FlagPoller::new(config, cache.clone());
    poller.start();

    wait_for_calls(&etag_mock, 2);
    poller.stop();
    assert!(no_etag_mock.calls() >= 1);

    // Verify at least one request WITH If-None-Match was made (second+ poll)
    assert!(
        etag_mock.calls() >= 1,
        "Should have at least 1 request with If-None-Match header, got {}",
        etag_mock.calls()
    );

    // Verify cache has the flag (preserved after 304)
    assert!(
        cache.get_flag("test-flag").is_some(),
        "Flag should be in cache"
    );
}

#[test]
fn test_sync_304_preserves_cache() {
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
    let etag_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/flags/definitions/")
            .query_param("send_cohorts", "")
            .is_true(|req| {
                // Match only if If-None-Match header exists with the correct value
                req.headers_vec().iter().any(|(name, value)| {
                    name.eq_ignore_ascii_case("if-none-match") && value == "\"sync-v1\""
                })
            });
        then.status(304);
    });

    // Mock for requests WITHOUT If-None-Match -> returns 200 with ETag
    server.mock(|when, then| {
        when.method(GET)
            .path("/flags/definitions/")
            .query_param("send_cohorts", "");
        then.status(200)
            .header("ETag", "\"sync-v1\"")
            .json_body(mock_flags);
    });

    let cache = FlagCache::new();
    let config = LocalEvaluationConfig {
        secret_key: "test_personal_key".to_string(),
        project_api_key: "test_project_key".to_string(),
        api_host: server.base_url(),
        poll_interval: Duration::from_millis(100),
        request_timeout: Duration::from_secs(5),
    };

    let mut poller = FlagPoller::new(config, cache.clone());
    poller.start();

    // Verify flag is in cache after initial load
    assert!(
        cache.get_flag("preserved-flag").is_some(),
        "Flag should be loaded initially"
    );

    wait_for_calls(&etag_mock, 2);

    poller.stop();

    // Verify 304 mock was hit (proves If-None-Match was sent and 304 was received)
    assert!(
        etag_mock.calls() >= 1,
        "Should have received at least one 304 response, got {} hits",
        etag_mock.calls()
    );

    // Flag should still be present after 304 response (cache preserved)
    assert!(
        cache.get_flag("preserved-flag").is_some(),
        "Flag should remain in cache after 304 response"
    );
}

#[test]
fn test_sync_no_etag_from_server() {
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

    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/flags/definitions/")
            .header_missing("if-none-match");
        then.status(200).json_body(mock_flags);
    });
    let unexpected_etag = server.mock(|when, then| {
        when.method(GET)
            .path("/flags/definitions/")
            .header_exists("if-none-match");
        then.status(400);
    });

    let cache = FlagCache::new();
    let config = LocalEvaluationConfig {
        secret_key: "test_personal_key".to_string(),
        project_api_key: "test_project_key".to_string(),
        api_host: server.base_url(),
        poll_interval: Duration::from_millis(50),
        request_timeout: Duration::from_secs(5),
    };

    let mut poller = FlagPoller::new(config, cache.clone());
    poller.start();

    wait_for_calls(&mock, 3);

    poller.stop();
    unexpected_etag.assert_calls(0);

    // Should have made multiple requests (initial + polls), all without If-None-Match
    assert!(
        mock.calls() >= 2,
        "Should have made multiple requests without ETag"
    );

    // Cache should have the flag
    assert!(
        cache.get_flag("no-etag-flag").is_some(),
        "Flag should be in cache"
    );
}

// ---- Group-targeted and mixed-targeting tests ----

fn mixed_flag() -> FeatureFlag {
    FeatureFlag {
        key: "mixed-flag".to_string(),
        active: true,
        has_experiment: None,
        filters: FeatureFlagFilters {
            groups: vec![
                // Group condition: company plan == enterprise
                FeatureFlagCondition {
                    properties: vec![Property {
                        key: "plan".to_string(),
                        value: json!("enterprise"),
                        operator: "exact".to_string(),
                        property_type: Some("group".to_string()),
                    }],
                    rollout_percentage: Some(100.0),
                    variant: None,
                    aggregation_group_type_index: Some(0),
                },
                // Person condition: email == test@example.com
                FeatureFlagCondition {
                    properties: vec![Property {
                        key: "email".to_string(),
                        value: json!("test@example.com"),
                        operator: "exact".to_string(),
                        property_type: Some("person".to_string()),
                    }],
                    rollout_percentage: Some(100.0),
                    variant: None,
                    aggregation_group_type_index: None,
                },
            ],
            multivariate: None,
            payloads: HashMap::new(),
            aggregation_group_type_index: None, // null = mixed
            early_exit: false,
        },
    }
}

fn only_group_flag() -> FeatureFlag {
    FeatureFlag {
        key: "only-group-flag".to_string(),
        active: true,
        has_experiment: None,
        filters: FeatureFlagFilters {
            groups: vec![FeatureFlagCondition {
                properties: vec![Property {
                    key: "plan".to_string(),
                    value: json!("enterprise"),
                    operator: "exact".to_string(),
                    property_type: Some("group".to_string()),
                }],
                rollout_percentage: Some(100.0),
                variant: None,
                aggregation_group_type_index: None,
            }],
            multivariate: None,
            payloads: HashMap::new(),
            // Pure group flag at the flag level
            aggregation_group_type_index: Some(0),
            early_exit: false,
        },
    }
}

fn cache_with(flag: FeatureFlag) -> FlagCache {
    let cache = FlagCache::new();
    let mut group_type_mapping = HashMap::new();
    group_type_mapping.insert("0".to_string(), "company".to_string());
    let mut response = LocalEvaluationResponse::new(vec![flag]);
    response.group_type_mapping = group_type_mapping;
    cache.update(response);
    cache
}

#[test]
fn test_pure_group_flag_matches_with_group_passed_in() {
    let evaluator = LocalEvaluator::new(cache_with(only_group_flag()));
    let mut groups = HashMap::new();
    groups.insert("company".to_string(), "acme".to_string());
    let mut group_props = HashMap::new();
    let mut acme_props = HashMap::new();
    acme_props.insert("plan".to_string(), json!("enterprise"));
    group_props.insert("company".to_string(), acme_props);

    let result = evaluator
        .evaluate_flag(
            "only-group-flag",
            "any-distinct-id",
            &HashMap::new(),
            &groups,
            &group_props,
        )
        .unwrap();
    assert_eq!(result, Some(FlagValue::Boolean(true)));
}

#[test]
fn test_pure_group_flag_returns_false_when_groups_not_passed() {
    // Pure group flag with no groups passed in: skip the condition, return false.
    let evaluator = LocalEvaluator::new(cache_with(only_group_flag()));
    let result = evaluator
        .evaluate_flag(
            "only-group-flag",
            "any-distinct-id",
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        )
        .unwrap();
    assert_eq!(result, Some(FlagValue::Boolean(false)));
}

#[test]
fn test_mixed_flag_person_condition_matches_when_no_groups() {
    let evaluator = LocalEvaluator::new(cache_with(mixed_flag()));
    let mut person = HashMap::new();
    person.insert("email".to_string(), json!("test@example.com"));

    let result = evaluator
        .evaluate_flag(
            "mixed-flag",
            "user-1",
            &person,
            &HashMap::new(),
            &HashMap::new(),
        )
        .unwrap();
    assert_eq!(result, Some(FlagValue::Boolean(true)));
}

#[test]
fn test_mixed_flag_group_condition_matches_with_group_props() {
    let evaluator = LocalEvaluator::new(cache_with(mixed_flag()));
    let mut groups = HashMap::new();
    groups.insert("company".to_string(), "acme".to_string());
    let mut group_props = HashMap::new();
    let mut acme = HashMap::new();
    acme.insert("plan".to_string(), json!("enterprise"));
    group_props.insert("company".to_string(), acme);
    let mut person = HashMap::new();
    person.insert("email".to_string(), json!("nope@example.com"));

    let result = evaluator
        .evaluate_flag("mixed-flag", "user-2", &person, &groups, &group_props)
        .unwrap();
    assert_eq!(result, Some(FlagValue::Boolean(true)));
}

#[test]
fn test_mixed_flag_no_match_when_both_fail() {
    let evaluator = LocalEvaluator::new(cache_with(mixed_flag()));
    let mut groups = HashMap::new();
    groups.insert("company".to_string(), "acme".to_string());
    let mut group_props = HashMap::new();
    let mut acme = HashMap::new();
    acme.insert("plan".to_string(), json!("free"));
    group_props.insert("company".to_string(), acme);
    let mut person = HashMap::new();
    person.insert("email".to_string(), json!("nope@example.com"));

    let result = evaluator
        .evaluate_flag("mixed-flag", "user-3", &person, &groups, &group_props)
        .unwrap();
    assert_eq!(result, Some(FlagValue::Boolean(false)));
}

#[test]
fn test_mixed_flag_only_group_condition_no_groups_returns_false() {
    // Mixed flag with only group conditions and no groups passed: skip, return false.
    let flag = FeatureFlag {
        key: "mixed-only-group".to_string(),
        active: true,
        has_experiment: None,
        filters: FeatureFlagFilters {
            groups: vec![FeatureFlagCondition {
                properties: vec![Property {
                    key: "plan".to_string(),
                    value: json!("enterprise"),
                    operator: "exact".to_string(),
                    property_type: Some("group".to_string()),
                }],
                rollout_percentage: Some(100.0),
                variant: None,
                aggregation_group_type_index: Some(0),
            }],
            multivariate: None,
            payloads: HashMap::new(),
            aggregation_group_type_index: None,
            early_exit: false,
        },
    };
    let evaluator = LocalEvaluator::new(cache_with(flag));
    let result = evaluator
        .evaluate_flag(
            "mixed-only-group",
            "user-1",
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        )
        .unwrap();
    assert_eq!(result, Some(FlagValue::Boolean(false)));
}

#[test]
fn test_group_condition_uses_group_key_for_bucketing() {
    // Group condition at 50% rollout. Use two groups whose hash on
    // ("rollout-flag", group_key) straddles 0.5, with a distinct_id whose hash
    // is also out-of-bucket. If the matcher regressed to bucketing on
    // distinct_id, the in-bucket assertion would fail.
    let flag = FeatureFlag {
        key: "rollout-flag".to_string(),
        active: true,
        has_experiment: None,
        filters: FeatureFlagFilters {
            groups: vec![FeatureFlagCondition {
                properties: vec![],
                rollout_percentage: Some(50.0),
                variant: None,
                aggregation_group_type_index: None,
            }],
            multivariate: None,
            payloads: HashMap::new(),
            aggregation_group_type_index: Some(0),
            early_exit: false,
        },
    };
    let evaluator = LocalEvaluator::new(cache_with(flag));
    let distinct_id = "user-0"; // hash > 0.5 against "rollout-flag"

    // Pre-computed: hash("rollout-flag.company-7") ~ 0.118 → in bucket
    let mut groups_in = HashMap::new();
    groups_in.insert("company".to_string(), "company-7".to_string());
    let mut group_props = HashMap::new();
    group_props.insert("company".to_string(), HashMap::new());
    let result = evaluator
        .evaluate_flag(
            "rollout-flag",
            distinct_id,
            &HashMap::new(),
            &groups_in,
            &group_props,
        )
        .unwrap();
    assert_eq!(result, Some(FlagValue::Boolean(true)));

    // Pre-computed: hash("rollout-flag.company-2") ~ 0.803 → out of bucket
    let mut groups_out = HashMap::new();
    groups_out.insert("company".to_string(), "company-2".to_string());
    let result = evaluator
        .evaluate_flag(
            "rollout-flag",
            distinct_id,
            &HashMap::new(),
            &groups_out,
            &group_props,
        )
        .unwrap();
    assert_eq!(result, Some(FlagValue::Boolean(false)));
}

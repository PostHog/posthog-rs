#![allow(deprecated)]

use httpmock::prelude::*;
use posthog_rs::{
    ClientOptions, ClientOptionsBuilder, EvaluateFlagsOptions, Event, FeatureFlagEvaluations,
    FeatureFlagsResponse, FlagValue,
};
use serde_json::{json, Value};

fn failed_response(error_bit: Option<bool>) -> Value {
    let mut body = json!({
        "flags": {
            "failed": {"key": "failed", "enabled": false, "failed": true},
            "failed-variant": {
                "key": "failed-variant", "enabled": true, "variant": "placeholder", "failed": true,
                "metadata": {"id": 1, "version": 1, "payload": {"copy": "remote"}}
            },
            "enabled": {"key": "enabled", "enabled": true},
            "disabled": {"key": "disabled", "enabled": false, "failed": false},
            "variant": {
                "key": "variant", "enabled": true, "variant": "blue",
                "metadata": {"id": 2, "version": 1, "payload": {"copy": "healthy"}}
            },
            "local-enabled": {"key": "local-enabled", "enabled": false, "failed": true},
            "local-disabled": {"key": "local-disabled", "enabled": false, "failed": true},
            "local-variant": {
                "key": "local-variant", "enabled": true, "variant": "placeholder", "failed": true,
                "metadata": {"id": 3, "version": 1, "payload": {"copy": "remote"}}
            }
        },
        "featureFlags": {"failed": false, "failed-variant": "placeholder"},
        "featureFlagPayloads": {"failed-variant": {"copy": "mirror"}}
    });
    if let Some(error_bit) = error_bit {
        body["errorsWhileComputingFlags"] = json!(error_bit);
    }
    body
}

#[test]
fn detailed_response_omits_failed_values_payloads_and_flat_mirrors() {
    for error_bit in [None, Some(false), Some(true)] {
        let response: FeatureFlagsResponse =
            serde_json::from_value(failed_response(error_bit)).unwrap();
        let (flags, payloads) = response.normalize();
        assert_eq!(flags.len(), 3);
        assert_eq!(flags["enabled"], FlagValue::Boolean(true));
        assert_eq!(flags["disabled"], FlagValue::Boolean(false));
        assert_eq!(flags["variant"], FlagValue::String("blue".into()));
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads["variant"], json!({"copy": "healthy"}));
    }
}

#[test]
fn all_failed_is_empty_and_legacy_flat_false_is_unchanged() {
    let response: FeatureFlagsResponse = serde_json::from_value(json!({
        "flags": {"failed": {"key": "failed", "enabled": false, "failed": true}}
    }))
    .unwrap();
    let (flags, payloads) = response.normalize();
    assert!(flags.is_empty());
    assert!(payloads.is_empty());

    let response: FeatureFlagsResponse = serde_json::from_value(json!({
        "featureFlags": {"disabled": false},
        "featureFlagPayloads": {"disabled": {"legacy": true}}
    }))
    .unwrap();
    let (flags, payloads) = response.normalize();
    assert_eq!(flags["disabled"], FlagValue::Boolean(false));
    assert_eq!(payloads["disabled"], json!({"legacy": true}));
}

fn setup_server(error_bit: Option<bool>, local: bool) -> (MockServer, ClientOptions) {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(failed_response(error_bit));
    });
    server.mock(|when, then| {
        when.method(GET).path("/flags/definitions/");
        then.status(200).json_body(json!({
            "flags": [
                {"key": "local-enabled", "active": true,
                 "filters": {"groups": [{"properties": [], "rollout_percentage": 100}]}},
                {"key": "local-disabled", "active": false},
                {"key": "local-variant", "active": true, "filters": {
                    "groups": [{"properties": [], "rollout_percentage": 100, "variant": "blue"}],
                    "multivariate": {"variants": [{"key": "blue", "rollout_percentage": 100}]},
                    "payloads": {"blue": {"copy": "local"}}
                }},
                {"key": "failed", "active": true, "filters": {"groups": [{
                    "properties": [{"key": "unknown", "operator": "future", "value": 1}],
                    "rollout_percentage": 100
                }]}}
            ],
            "group_type_mapping": {}, "cohorts": {}
        }));
    });
    server.mock(|when, then| {
        #[cfg(feature = "capture-v1")]
        let path = "/i/v1/analytics/events";
        #[cfg(not(feature = "capture-v1"))]
        let path = "/batch/";
        when.method(POST).path(path);
        then.status(200).json_body(json!({"results": {}}));
    });
    let options = ClientOptionsBuilder::default()
        .api_key("test-key".to_string())
        .host(server.base_url())
        .enable_local_evaluation(local)
        .secret_key("test-secret".to_string())
        .poll_interval_seconds(3600)
        .build()
        .unwrap();
    (server, options)
}

fn assert_snapshot(snapshot: &FeatureFlagEvaluations, local: bool) {
    assert!(!snapshot.keys().iter().any(|key| key.starts_with("failed")));
    assert_eq!(snapshot.get_flag("failed"), None);
    assert_eq!(snapshot.get_flag("failed-variant"), None);
    assert_eq!(snapshot.get_flag_payload("failed-variant"), None);
    assert_eq!(
        snapshot.get_flag("disabled"),
        Some(FlagValue::Boolean(false))
    );
    if local {
        assert_eq!(
            snapshot.get_flag("local-enabled"),
            Some(FlagValue::Boolean(true))
        );
        assert_eq!(
            snapshot.get_flag("local-disabled"),
            Some(FlagValue::Boolean(false))
        );
        assert_eq!(
            snapshot.get_flag("local-variant"),
            Some(FlagValue::String("blue".into()))
        );
        assert_eq!(
            snapshot.get_flag_payload("local-variant"),
            Some(json!({"copy": "local"}))
        );
    } else {
        assert_eq!(snapshot.keys().len(), 3);
    }
    let mut event = Event::new("test", "user");
    event.with_flags(snapshot);
    assert!(!event.properties().contains_key("$feature/failed"));
    assert!(!event.properties().contains_key("$feature/failed-variant"));
    if local {
        assert_eq!(event.properties()["$feature/local-variant"], json!("blue"));
    }
}

#[cfg(not(feature = "async-client"))]
#[test]
fn blocking_failed_direct_reads_and_snapshots() {
    for local in [false, true] {
        for error_bit in [None, Some(false), Some(true)] {
            let (_server, options) = setup_server(error_bit, local);
            let client = posthog_rs::client(options);
            assert_eq!(
                client
                    .get_feature_flag("failed", "user", None, None, None)
                    .unwrap(),
                None
            );
            assert_eq!(
                client
                    .get_feature_flag("failed-variant", "user", None, None, None)
                    .unwrap(),
                None
            );
            assert!(!client
                .is_feature_enabled("failed", "user", None, None, None)
                .unwrap());
            assert_eq!(
                client
                    .get_feature_flag_payload("failed-variant", "user")
                    .unwrap(),
                None
            );
            let (flags, payloads) = client.get_feature_flags("user", None, None, None).unwrap();
            assert_eq!(flags.len(), 3);
            assert_eq!(payloads.len(), 1);
            let snapshot = client
                .evaluate_flags("user", EvaluateFlagsOptions::default())
                .unwrap();
            assert_snapshot(&snapshot, local);
            client.flush();
        }
    }
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn async_failed_direct_reads_and_snapshots() {
    for local in [false, true] {
        for error_bit in [None, Some(false), Some(true)] {
            let (_server, options) = setup_server(error_bit, local);
            let client = posthog_rs::client(options).await;
            assert_eq!(
                client
                    .get_feature_flag("failed", "user", None, None, None)
                    .await
                    .unwrap(),
                None
            );
            assert_eq!(
                client
                    .get_feature_flag("failed-variant", "user", None, None, None)
                    .await
                    .unwrap(),
                None
            );
            assert!(!client
                .is_feature_enabled("failed", "user", None, None, None)
                .await
                .unwrap());
            assert_eq!(
                client
                    .get_feature_flag_payload("failed-variant", "user")
                    .await
                    .unwrap(),
                None
            );
            let (flags, payloads) = client
                .get_feature_flags("user", None, None, None)
                .await
                .unwrap();
            assert_eq!(flags.len(), 3);
            assert_eq!(payloads.len(), 1);
            let snapshot = client
                .evaluate_flags("user", EvaluateFlagsOptions::default())
                .await
                .unwrap();
            assert_snapshot(&snapshot, local);
            client.flush().await;
        }
    }
}

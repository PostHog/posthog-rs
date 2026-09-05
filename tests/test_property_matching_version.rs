use posthog_rs::{FlagCache, FlagValue, LocalEvaluationResponse, LocalEvaluator};
use serde_json::{json, Value};
use std::collections::HashMap;

fn definitions(version: Option<i64>, filter: Value, operator: &str) -> LocalEvaluationResponse {
    let mut response = json!({
        "flags": [{"key": "person", "active": true, "filters": {"groups": [{
            "properties": [{"key": "value", "value": filter, "operator": operator}]
        }]}}]
    });
    if let Some(version) = version {
        response["property_matching_version"] = json!(version);
    }
    // Exercise the actual wire envelope, not a manually constructed cache entry.
    serde_json::from_value(response).unwrap()
}

fn assert_paths(evaluator: &LocalEvaluator, property: Value, expected: bool) {
    let properties = HashMap::from([("value".into(), property)]);
    let groups = HashMap::new();
    let group_properties = HashMap::new();
    assert_eq!(
        evaluator
            .evaluate_flag("person", "user", &properties, &groups, &group_properties)
            .unwrap(),
        Some(FlagValue::Boolean(expected)),
        "full path"
    );
    assert_eq!(
        evaluator
            .evaluate_flag_simple("person", "user", &properties, &groups, &group_properties)
            .unwrap(),
        Some(FlagValue::Boolean(expected)),
        "simple path"
    );
    assert_eq!(
        evaluator.evaluate_all_flags("user", &properties, &groups, &group_properties)["person"]
            .as_ref()
            .unwrap(),
        &FlagValue::Boolean(expected),
        "all flags path"
    );
}

#[test]
fn property_matching_version_wire_matrix() {
    let cases = [
        (json!(false), json!("banana"), true, false),
        (json!(false), json!(0), true, false),
        (json!(["true", "false"]), json!("true"), false, true),
        (json!(["true", "false"]), json!("pro"), true, false),
        (json!([]), json!(true), true, true),
        (json!([]), json!([]), true, true),
        (json!(true), json!([true]), true, false),
        (json!(true), json!([]), true, false),
        (json!(false), json!("FALSE"), true, true),
        (json!(false), Value::Null, true, false),
        (json!(false), json!(""), true, false),
        (json!([]), json!([true, "TRUE", [true, []]]), true, true),
        (json!([]), json!(false), false, false),
        (json!([]), json!(1), false, false),
        (json!([]), json!("banana"), false, false),
        (json!([]), json!([true, [false]]), false, false),
        (json!([false, "PRO"]), json!("pro"), true, true),
        (json!([true, "pro"]), json!("TRUE"), true, true),
        (json!([[true], "PRO"]), json!([true]), true, true),
        (json!(["FREE", "PRO"]), json!("pro"), true, true),
        (json!("ÄBC"), json!("äbc"), true, true),
        (json!([1, "pro"]), json!("1"), true, true),
        (Value::Null, Value::Null, true, true),
    ];
    for version in [None, Some(1), Some(2), Some(0), Some(3)] {
        for (filter, property, legacy, explicit) in &cases {
            for operator in ["exact", "is_not"] {
                let cache = FlagCache::new();
                cache.update(definitions(version, filter.clone(), operator));
                let expected = if version == Some(2) {
                    *explicit
                } else {
                    *legacy
                };
                assert_paths(
                    &LocalEvaluator::new(cache),
                    property.clone(),
                    expected != (operator == "is_not"),
                );
            }
        }
    }
}

#[test]
fn property_matching_version_only_reload_and_older_cache_roundtrip() {
    let cache = FlagCache::new();
    let evaluator = LocalEvaluator::new(cache.clone());
    for version in [Some(1), Some(2), Some(1), Some(2), None] {
        let response = definitions(version, json!(false), "exact");
        let serialized = serde_json::to_string(&response).unwrap();
        cache.update(serde_json::from_str(&serialized).unwrap());
        assert_paths(&evaluator, json!("banana"), version != Some(2));
    }
}

#[test]
fn property_matching_version_missing_property_is_inconclusive() {
    for version in [None, Some(1), Some(2)] {
        for operator in ["exact", "is_not"] {
            let cache = FlagCache::new();
            cache.update(definitions(version, json!(false), operator));
            let evaluator = LocalEvaluator::new(cache);
            assert!(evaluator
                .evaluate_flag(
                    "person",
                    "user",
                    &HashMap::new(),
                    &HashMap::new(),
                    &HashMap::new()
                )
                .is_err());
            assert!(evaluator
                .evaluate_flag_simple(
                    "person",
                    "user",
                    &HashMap::new(),
                    &HashMap::new(),
                    &HashMap::new()
                )
                .is_err());
        }
    }
}

#[test]
fn property_matching_version_reaches_groups_recursive_cohorts_and_dependencies() {
    let cache = FlagCache::new();
    let evaluator = LocalEvaluator::new(cache.clone());
    for version in [None, Some(1), Some(2), Some(1), Some(2), None] {
        let mut response =
            serde_json::to_value(definitions(version, json!(false), "exact")).unwrap();
        let mut group = response["flags"][0].clone();
        group["key"] = json!("group");
        group["filters"]["aggregation_group_type_index"] = json!(0);
        response["flags"].as_array_mut().unwrap().push(group);
        response["flags"].as_array_mut().unwrap().push(json!({
            "key": "cohort", "active": true, "filters": {"groups": [{"properties": [
                {"key": "id", "value": "outer", "type": "cohort"}
            ]}]}
        }));
        response["flags"].as_array_mut().unwrap().push(json!({
            "key": "dependency", "active": true, "filters": {"groups": [{"properties": [
                {"key": "$feature/group", "value": true, "operator": "exact"}
            ]}]}
        }));
        response["group_type_mapping"] = json!({"0": "company"});
        response["cohorts"] = json!({
            "outer": {"type": "AND", "values": [{"type": "cohort", "value": "inner"}]},
            "inner": {"type": "OR", "values": [{"type": "AND", "values": [
                {"key": "value", "value": false, "operator": "exact", "type": "person"}
            ]}]}
        });
        cache.update(serde_json::from_value(response).unwrap());
        let properties = HashMap::from([("value".into(), json!("banana"))]);
        let groups = HashMap::from([("company".into(), "acme".into())]);
        let group_properties = HashMap::from([("company".into(), properties.clone())]);
        let expected = FlagValue::Boolean(version != Some(2));
        for key in ["person", "group", "cohort", "dependency"] {
            assert_eq!(
                evaluator
                    .evaluate_flag(key, "user", &properties, &groups, &group_properties)
                    .unwrap(),
                Some(expected.clone()),
                "{key}, version {version:?}"
            );
            assert_eq!(
                evaluator.evaluate_all_flags("user", &properties, &groups, &group_properties)[key]
                    .as_ref()
                    .unwrap(),
                &expected
            );
        }
        assert_eq!(
            evaluator
                .evaluate_flag_simple("group", "user", &properties, &groups, &group_properties)
                .unwrap(),
            Some(expected)
        );
    }
}

fn poller_config(server: &httpmock::MockServer) -> posthog_rs::LocalEvaluationConfig {
    posthog_rs::LocalEvaluationConfig {
        personal_api_key: "personal-key".into(),
        project_api_key: "project-key".into(),
        api_host: server.base_url(),
        poll_interval: std::time::Duration::from_millis(20),
        request_timeout: std::time::Duration::from_secs(5),
    }
}

fn mock_definitions(server: &httpmock::MockServer, version: Option<i64>) -> httpmock::Mock<'_> {
    let mut body = serde_json::to_value(definitions(version, json!(false), "exact")).unwrap();
    // A fresh old-server response must really omit the field on the wire.
    if version.is_none() {
        body.as_object_mut()
            .unwrap()
            .remove("property_matching_version");
    }
    server.mock(|when, then| {
        when.method(httpmock::Method::GET)
            .path("/flags/definitions/")
            .query_param("send_cohorts", "")
            .header("Authorization", "Bearer personal-key")
            .header("X-PostHog-Project-Api-Key", "project-key");
        then.status(200)
            .header("ETag", "\"definitions\"")
            .json_body(body);
    })
}

#[test]
fn property_matching_version_blocking_poller_reload_failure_and_304() {
    let server = httpmock::MockServer::start();
    let cache = FlagCache::new();
    let evaluator = LocalEvaluator::new(cache.clone());
    let mut poller = posthog_rs::FlagPoller::new(poller_config(&server), cache);
    for version in [Some(1), Some(2), Some(1), Some(2), None, Some(2)] {
        let mut response = mock_definitions(&server, version);
        poller.load_flags().unwrap();
        response.assert_calls(1);
        assert_paths(&evaluator, json!("banana"), version != Some(2));
        response.delete();
    }
    for (status, body) in [(500, "unavailable"), (200, "invalid json")] {
        let mut response = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/flags/definitions/");
            then.status(status).body(body);
        });
        assert!(poller.load_flags().is_err());
        assert_paths(&evaluator, json!("banana"), false);
        response.delete();
    }
    let unchanged = server.mock(|when, then| {
        when.method(httpmock::Method::GET)
            .path("/flags/definitions/")
            .header("If-None-Match", "\"definitions\"");
        then.status(304);
    });
    let _response = mock_definitions(&server, Some(2));
    poller.start();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while unchanged.calls() == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    poller.stop();
    assert!(unchanged.calls() > 0, "poller must receive a real 304");
    assert_paths(&evaluator, json!("banana"), false);
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn property_matching_version_async_poller_reload_failure_and_304() {
    let server = httpmock::MockServer::start();
    let cache = FlagCache::new();
    let evaluator = LocalEvaluator::new(cache.clone());
    let mut poller = posthog_rs::AsyncFlagPoller::new(poller_config(&server), cache);
    for version in [Some(1), Some(2), Some(1), Some(2), None, Some(2)] {
        let mut response = mock_definitions(&server, version);
        poller.load_flags().await.unwrap();
        response.assert_calls(1);
        assert_paths(&evaluator, json!("banana"), version != Some(2));
        response.delete();
    }
    for (status, body) in [(500, "unavailable"), (200, "invalid json")] {
        let mut response = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/flags/definitions/");
            then.status(status).body(body);
        });
        assert!(poller.load_flags().await.is_err());
        assert_paths(&evaluator, json!("banana"), false);
        response.delete();
    }
    let unchanged = server.mock(|when, then| {
        when.method(httpmock::Method::GET)
            .path("/flags/definitions/")
            .header("If-None-Match", "\"definitions\"");
        then.status(304);
    });
    let _response = mock_definitions(&server, Some(2));
    poller.start().await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while unchanged.calls_async().await == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("poller must receive a real 304");
    poller.stop().await;
    assert_paths(&evaluator, json!("banana"), false);
}

#[test]
fn property_matching_version_public_helper_defaults_to_legacy() {
    let response = definitions(Some(2), json!(false), "exact");
    // Context-free callers have no definitions metadata; retain released behavior.
    assert_eq!(
        posthog_rs::match_feature_flag(
            &response.flags[0],
            "user",
            &HashMap::from([("value".into(), json!("banana"))]),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        )
        .unwrap(),
        FlagValue::Boolean(true)
    );
}

#[test]
fn property_matching_version_stays_with_definitions_during_concurrent_refresh() {
    let cache = FlagCache::new();
    let legacy = definitions(Some(1), json!(false), "exact");
    let explicit = definitions(Some(2), json!("banana"), "exact");
    // Both snapshots match. A legacy flag paired with the explicit version does not.
    cache.update(legacy.clone());
    let evaluator = LocalEvaluator::new(cache.clone());
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let writer_barrier = barrier.clone();
    let writer = std::thread::spawn(move || {
        writer_barrier.wait();
        for _ in 0..1000 {
            cache.update(explicit.clone());
            cache.update(legacy.clone());
        }
    });
    barrier.wait();
    for _ in 0..1000 {
        assert_paths(&evaluator, json!("banana"), true);
    }
    writer.join().unwrap();
}

//! Early-access features: the person's opt-in in `$feature_enrollment/<flag key>` decides
//! the flag before its release conditions (sdk-specs local-feature-flag-evaluator,
//! local-feature-flag-enrollment.feature).

use posthog::{
    FeatureFlagCondition, FeatureFlagFilters, FlagCache, FlagValue, LocalEvaluationResponse,
    LocalEvaluator,
};
use serde_json::{json, Value};
use std::collections::HashMap;

const ENROLLMENT_KEY: &str = "$feature_enrollment/beta-feature";

type Properties = HashMap<String, Value>;

fn enrollment(value: Value) -> Properties {
    HashMap::from([(ENROLLMENT_KEY.to_string(), value)])
}

fn everyone(rollout: f64) -> Value {
    json!([{"properties": [], "rollout_percentage": rollout}])
}

/// The early-access flag, with `filters` and `fields` merged over the defaults.
fn beta_feature(filters: Value, fields: Value) -> Value {
    let mut flag = json!({
        "key": "beta-feature",
        "active": true,
        "filters": {"feature_enrollment": true, "groups": everyone(100.0)}
    });
    for (key, value) in filters.as_object().unwrap() {
        flag["filters"][key] = value.clone();
    }
    for (key, value) in fields.as_object().unwrap() {
        flag[key] = value.clone();
    }
    flag
}

fn dependent(aggregation: Option<i32>) -> Value {
    json!({
        "key": "dependent",
        "active": true,
        "filters": {
            "aggregation_group_type_index": aggregation,
            "groups": [{
                "properties": [{"key": "beta-feature", "type": "flag", "value": true, "operator": "flag_evaluates_to", "dependency_chain": ["beta-feature"]}],
                "rollout_percentage": 100.0
            }]
        }
    })
}

fn definitions(flags: Vec<Value>, cohorts: Value) -> Value {
    json!({"flags": flags, "group_type_mapping": {"0": "company"}, "cohorts": cohorts})
}

fn evaluator(definitions: Value) -> LocalEvaluator {
    let response: LocalEvaluationResponse = serde_json::from_value(definitions).unwrap();
    let cache = FlagCache::new();
    cache.update(response);
    LocalEvaluator::new(cache)
}

/// `None` when the flag is inconclusive. Every way to evaluate it must agree.
fn evaluate(flags: Vec<Value>, key: &str, person_properties: &Properties) -> Option<FlagValue> {
    evaluate_in(flags, json!({}), key, person_properties, None)
}

fn evaluate_in(
    flags: Vec<Value>,
    cohorts: Value,
    key: &str,
    person_properties: &Properties,
    group_properties: Option<Properties>,
) -> Option<FlagValue> {
    let evaluator = evaluator(definitions(flags, cohorts));
    let mut groups = HashMap::new();
    let mut all_group_properties = HashMap::new();
    if let Some(properties) = group_properties {
        groups.insert("company".to_string(), "acme".to_string());
        all_group_properties.insert("company".to_string(), properties);
    }

    let single = evaluator
        .evaluate_flag(
            key,
            "user-123",
            person_properties,
            &groups,
            &all_group_properties,
        )
        .ok()
        .map(|value| value.expect("the flag is defined"));
    let bulk = evaluator
        .evaluate_all_flags(
            "user-123",
            person_properties,
            &groups,
            &all_group_properties,
        )
        .remove(key)
        .expect("the flag is defined")
        .ok();
    assert_eq!(single, bulk);
    if key == "beta-feature" {
        let simple = evaluator
            .evaluate_flag_simple(
                key,
                "user-123",
                person_properties,
                &groups,
                &all_group_properties,
            )
            .ok()
            .flatten();
        assert_eq!(single, simple);
    }
    single
}

#[test]
fn values_decide_before_release_conditions() {
    for (value, rollout, expected) in [
        (json!(true), 0.0, true),
        (json!("true"), 0.0, true),
        (json!(false), 100.0, false),
        (json!("false"), 100.0, false),
        (json!(1), 100.0, false),
        (json!(0), 100.0, false),
        (json!(null), 100.0, false),
        (json!("True"), 100.0, false),
        (json!(""), 100.0, false),
        (json!([]), 100.0, false),
        (json!({}), 100.0, false),
    ] {
        let flag = beta_feature(json!({"groups": everyone(rollout)}), json!({}));
        assert_eq!(
            evaluate(vec![flag], "beta-feature", &enrollment(value.clone())),
            Some(FlagValue::Boolean(expected)),
            "enrollment {value}"
        );
    }
}

#[test]
fn missing_enrollment_is_inconclusive() {
    let flag = beta_feature(json!({}), json!({}));
    assert_eq!(evaluate(vec![flag], "beta-feature", &HashMap::new()), None);
}

#[test]
fn an_inactive_flag_stays_off() {
    for continuity in [true, false] {
        let flag = beta_feature(
            json!({}),
            json!({"active": false, "ensure_experience_continuity": continuity}),
        );
        for properties in [HashMap::new(), enrollment(json!(true))] {
            assert_eq!(
                evaluate(vec![flag.clone()], "beta-feature", &properties),
                Some(FlagValue::Boolean(false))
            );
        }
    }
}

#[test]
fn a_false_null_or_absent_marker_keeps_ordinary_targeting() {
    let mut absent = beta_feature(json!({}), json!({}));
    absent["filters"]
        .as_object_mut()
        .unwrap()
        .remove("feature_enrollment");
    for flag in [
        beta_feature(json!({"feature_enrollment": false}), json!({})),
        beta_feature(json!({"feature_enrollment": null}), json!({})),
        absent,
        beta_feature(
            json!({"feature_enrollment": null}),
            json!({"ensure_experience_continuity": null}),
        ),
    ] {
        assert_eq!(
            evaluate(vec![flag], "beta-feature", &HashMap::new()),
            Some(FlagValue::Boolean(true))
        );
    }
}

#[test]
fn release_conditions_are_not_read() {
    // A cohort that is not in the definitions, so the condition can't be evaluated.
    let flag = beta_feature(
        json!({"groups": [{
            "properties": [{"key": "id", "value": 123, "type": "cohort"}],
            "rollout_percentage": 100.0
        }]}),
        json!({}),
    );
    for enrolled in [true, false] {
        assert_eq!(
            evaluate(
                vec![flag.clone()],
                "beta-feature",
                &enrollment(json!(enrolled))
            ),
            Some(FlagValue::Boolean(enrolled))
        );
    }
    assert_eq!(evaluate(vec![flag], "beta-feature", &HashMap::new()), None);
}

#[test]
fn a_multivariate_flag_is_boolean() {
    let flag = beta_feature(
        json!({"multivariate": {"variants": [{"key": "test", "rollout_percentage": 100.0}]}}),
        json!({}),
    );
    assert_eq!(
        evaluate(vec![flag], "beta-feature", &enrollment(json!(true))),
        Some(FlagValue::Boolean(true))
    );
}

#[test]
fn experience_continuity_stays_inconclusive() {
    for groups in [
        everyone(100.0),
        json!([{
            "properties": [{"key": "id", "value": 123, "type": "cohort"}],
            "rollout_percentage": 100.0
        }]),
    ] {
        let flag = beta_feature(
            json!({"groups": groups}),
            json!({"ensure_experience_continuity": true}),
        );
        for properties in [
            HashMap::new(),
            enrollment(json!(true)),
            enrollment(json!(false)),
        ] {
            assert_eq!(
                evaluate(vec![flag.clone()], "beta-feature", &properties),
                None
            );
        }
    }
}

#[test]
fn a_group_flag_reads_the_persons_enrollment() {
    let flag = beta_feature(
        json!({"aggregation_group_type_index": 0, "groups": everyone(0.0)}),
        json!({}),
    );
    let evaluate = |person: Properties, group: Option<Properties>| {
        evaluate_in(
            vec![flag.clone()],
            json!({}),
            "beta-feature",
            &person,
            group,
        )
    };

    // No group context: the person's enrollment still decides.
    assert_eq!(
        evaluate(enrollment(json!(true)), None),
        Some(FlagValue::Boolean(true))
    );
    // Group properties can't turn the person's opt-out into enrollment, or stand in for it.
    assert_eq!(
        evaluate(enrollment(json!(false)), Some(enrollment(json!(true)))),
        Some(FlagValue::Boolean(false))
    );
    assert_eq!(
        evaluate(HashMap::new(), Some(enrollment(json!(true)))),
        None
    );
}

#[test]
fn a_dependent_flag_reads_the_persons_enrollment() {
    for dependent in [dependent(None), dependent(Some(0))] {
        for (person, expected) in [
            (enrollment(json!(true)), Some(FlagValue::Boolean(true))),
            (enrollment(json!(false)), Some(FlagValue::Boolean(false))),
            (HashMap::new(), None),
        ] {
            // The group's properties say the opposite.
            let group = enrollment(json!(expected != Some(FlagValue::Boolean(true))));
            assert_eq!(
                evaluate_in(
                    vec![beta_feature(json!({}), json!({})), dependent.clone()],
                    json!({}),
                    "dependent",
                    &person,
                    Some(group),
                ),
                expected,
                "{dependent}"
            );
        }
    }
}

#[test]
fn a_dependency_with_experience_continuity_is_inconclusive() {
    let flag = beta_feature(json!({}), json!({"ensure_experience_continuity": true}));
    assert_eq!(
        evaluate(
            vec![flag, dependent(None)],
            "dependent",
            &enrollment(json!(true))
        ),
        None
    );
}

#[test]
fn definitions_keep_the_marker_through_serialization() {
    let flags = vec![
        beta_feature(json!({}), json!({"ensure_experience_continuity": true})),
        dependent(None),
    ];
    let response: LocalEvaluationResponse =
        serde_json::from_value(definitions(flags, json!({}))).unwrap();

    let sent = serde_json::to_value(&response).unwrap();
    assert_eq!(sent["flags"][0]["filters"]["feature_enrollment"], true);
    assert_eq!(sent["flags"][0]["ensure_experience_continuity"], true);
    assert_eq!(
        sent["flags"][1].get("ensure_experience_continuity"),
        None,
        "left out when unset"
    );
    assert_eq!(
        sent["flags"][1]["filters"].get("feature_enrollment"),
        None,
        "left out when unset"
    );

    let cache = FlagCache::new();
    cache.update(serde_json::from_value(sent).unwrap());
    let result = LocalEvaluator::new(cache).evaluate_flag(
        "beta-feature",
        "user-123",
        &enrollment(json!(true)),
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(result.is_err());
}

#[test]
fn definitions_keep_every_other_field_through_serialization() {
    let mut full = beta_feature(
        json!({
            "groups": [{
                "properties": [],
                "rollout_percentage": 50.0,
                "variant": "test",
                "aggregation_group_type_index": 1
            }],
            "multivariate": {"variants": [{"key": "test", "rollout_percentage": 100.0}]},
            "payloads": {"test": {"color": "blue"}},
            "aggregation_group_type_index": 2,
            "early_exit": true
        }),
        json!({"has_experiment": true}),
    );
    let response: LocalEvaluationResponse =
        serde_json::from_value(definitions(vec![full.clone()], json!({}))).unwrap();

    let sent = serde_json::to_value(&response).unwrap();
    assert_eq!(sent["flags"][0], full);

    full["filters"]
        .as_object_mut()
        .unwrap()
        .remove("feature_enrollment");
    assert_eq!(serde_json::to_value(&response.flags[0]).unwrap(), full);
}

#[test]
fn a_cleared_cache_forgets_the_marker() {
    let evaluator = evaluator(definitions(
        vec![beta_feature(json!({}), json!({}))],
        json!({}),
    ));
    evaluator.cache().clear();
    let mut plain = beta_feature(json!({"feature_enrollment": false}), json!({}));
    plain["filters"]["groups"] = everyone(100.0);
    let response: LocalEvaluationResponse =
        serde_json::from_value(definitions(vec![plain], json!({}))).unwrap();
    evaluator.cache().update(response);

    let result = evaluator.evaluate_flag(
        "beta-feature",
        "user-123",
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    );
    assert_eq!(result.unwrap(), Some(FlagValue::Boolean(true)));
}

/// `1.x` callers build these with a literal, so they can't gain a required field.
#[test]
fn the_public_filters_can_still_be_built_with_a_literal() {
    let filters = FeatureFlagFilters {
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
    };
    assert_eq!(filters.groups.len(), 1);
}

#[cfg(feature = "async-client")]
mod client {
    use super::*;
    use httpmock::prelude::*;
    use posthog::{ClientOptionsBuilder, EvaluateFlagsOptions};

    async fn client(server: &MockServer, flags: Vec<Value>) -> posthog::Client {
        server.mock(|when, then| {
            when.method(GET).path("/flags/definitions/");
            then.status(200)
                .json_body(definitions(flags.clone(), json!({})));
        });
        let options = ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .host(server.base_url())
            .secret_key("phx_test".to_string())
            .enable_local_evaluation(true)
            .build()
            .unwrap();
        posthog::client(options).await
    }

    fn options(person_properties: Properties, only_evaluate_locally: bool) -> EvaluateFlagsOptions {
        let mut options = EvaluateFlagsOptions::default();
        options.person_properties = Some(person_properties);
        options.only_evaluate_locally = only_evaluate_locally;
        options.flag_keys = Some(vec!["beta-feature".to_string()]);
        options
    }

    #[tokio::test]
    async fn enrollment_resolves_locally_with_its_payload() {
        let server = MockServer::start_async().await;
        let remote = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(500);
        });
        let flag = beta_feature(
            json!({"groups": everyone(0.0), "payloads": {"true": {"color": "blue"}}}),
            json!({}),
        );
        let client = client(&server, vec![flag]).await;

        let enrolled = client
            .evaluate_flags("user-123", options(enrollment(json!(true)), false))
            .await
            .unwrap();
        assert_eq!(
            enrolled.get_flag("beta-feature"),
            Some(FlagValue::Boolean(true))
        );
        assert_eq!(
            enrolled.get_flag_payload("beta-feature"),
            Some(json!({"color": "blue"}))
        );

        let opted_out = client
            .evaluate_flags("user-123", options(enrollment(json!(false)), false))
            .await
            .unwrap();
        assert_eq!(
            opted_out.get_flag("beta-feature"),
            Some(FlagValue::Boolean(false))
        );
        assert_eq!(opted_out.get_flag_payload("beta-feature"), None);
        remote.assert_calls(0);
    }

    #[tokio::test]
    async fn a_malformed_marker_skips_only_its_flag() {
        let server = MockServer::start_async().await;
        let malformed = json!({
            "key": "malformed",
            "active": true,
            "filters": {"feature_enrollment": "yes", "groups": everyone(100.0)}
        });
        let client = client(&server, vec![malformed, beta_feature(json!({}), json!({}))]).await;

        let flags = client
            .evaluate_flags("user-123", options(enrollment(json!(true)), true))
            .await
            .unwrap();
        assert_eq!(
            flags.get_flag("beta-feature"),
            Some(FlagValue::Boolean(true))
        );
    }

    #[tokio::test]
    async fn missing_enrollment_falls_back_to_the_server() {
        let server = MockServer::start_async().await;
        let remote = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(json!({
                "flags": {"beta-feature": {
                    "key": "beta-feature", "enabled": false, "variant": null,
                    "metadata": {"id": 1, "version": 1}
                }},
                "errorsWhileComputingFlags": false,
                "requestId": "request-1"
            }));
        });
        let client = client(&server, vec![beta_feature(json!({}), json!({}))]).await;

        let local = client
            .evaluate_flags("user-123", options(HashMap::new(), true))
            .await
            .unwrap();
        assert_eq!(local.get_flag("beta-feature"), None);
        remote.assert_calls(0);

        let flags = client
            .evaluate_flags("user-123", options(HashMap::new(), false))
            .await
            .unwrap();
        assert_eq!(
            flags.get_flag("beta-feature"),
            Some(FlagValue::Boolean(false))
        );
        remote.assert_calls(1);
    }
}

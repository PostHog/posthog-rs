//! A condition aggregated by a group with no usable key is skipped on its own, as the
//! flags service does: the flag's other conditions still decide it. A holdout on such a
//! flag can't be evaluated locally, since the service hashes an empty identifier for it.

use posthog::{FlagCache, FlagValue, LocalEvaluationResponse, LocalEvaluator};
use serde_json::{json, Value};
use std::collections::HashMap;

type Properties = HashMap<String, Value>;

/// A `company` flag whose only condition is an explicit person condition on `plan = pro`.
fn person_condition_on_company_flag() -> Value {
    json!({
        "key": "pro-plan",
        "active": true,
        "filters": {
            "aggregation_group_type_index": 0,
            "groups": [{
                "properties": [{"key": "plan", "value": "pro", "operator": "exact", "type": "person"}],
                "rollout_percentage": 100,
                "aggregation_group_type_index": null
            }]
        }
    })
}

/// A `company` flag with a `company` condition first and an explicit person condition
/// after it.
fn mixed_company_flag() -> Value {
    json!({
        "key": "mixed",
        "active": true,
        "filters": {
            "aggregation_group_type_index": 0,
            "groups": [
                {
                    "properties": [{"key": "tier", "value": "enterprise", "operator": "exact", "type": "group", "group_type_index": 0}],
                    "rollout_percentage": 100
                },
                {
                    "properties": [{"key": "plan", "value": "pro", "operator": "exact", "type": "person"}],
                    "rollout_percentage": 100,
                    "aggregation_group_type_index": null
                }
            ]
        }
    })
}

/// A `company` flag with only `company` conditions, and the given holdout.
fn company_flag(holdout: Option<Value>) -> Value {
    let mut flag = json!({
        "key": "company-flag",
        "active": true,
        "filters": {
            "aggregation_group_type_index": 0,
            "groups": [{"properties": [], "rollout_percentage": 100}]
        }
    });
    if let Some(holdout) = holdout {
        flag["filters"]["holdout"] = holdout;
    }
    flag
}

/// A person flag that depends on `parent`.
fn depends_on(parent: &str) -> Value {
    json!({
        "key": "dependent",
        "active": true,
        "filters": {
            "groups": [{
                "properties": [{"key": format!("$feature/{parent}"), "value": true, "operator": "exact", "type": "flag"}],
                "rollout_percentage": 100
            }]
        }
    })
}

fn definitions(flags: Vec<Value>) -> Value {
    json!({"flags": flags, "group_type_mapping": {"0": "company"}, "cohorts": {}})
}

fn evaluator(definitions: Value) -> LocalEvaluator {
    let response: LocalEvaluationResponse = serde_json::from_value(definitions).unwrap();
    let cache = FlagCache::new();
    cache.update(response);
    LocalEvaluator::new(cache)
}

fn plan(plan: &str) -> Properties {
    HashMap::from([("plan".to_string(), json!(plan))])
}

/// No `company` group, and a `company` group with an empty key.
fn groups_without_a_usable_key() -> [HashMap<String, String>; 2] {
    [
        HashMap::new(),
        HashMap::from([("company".to_string(), String::new())]),
    ]
}

/// `None` when the flag is inconclusive. Every way to evaluate it must agree.
fn evaluate(
    definitions: Value,
    key: &str,
    person_properties: &Properties,
    groups: &HashMap<String, String>,
) -> Option<FlagValue> {
    let evaluator = evaluator(definitions);
    let group_properties = HashMap::new();
    let single = evaluator
        .evaluate_flag(key, "user-1", person_properties, groups, &group_properties)
        .ok()
        .map(|value| value.expect("the flag is defined"));
    let bulk = evaluator
        .evaluate_all_flags("user-1", person_properties, groups, &group_properties)
        .remove(key)
        .expect("the flag is defined")
        .ok();
    assert_eq!(
        single, bulk,
        "evaluate_flag and evaluate_all_flags for {key}"
    );
    if !key.starts_with("dependent") {
        let simple = evaluator
            .evaluate_flag_simple(key, "user-1", person_properties, groups, &group_properties)
            .ok()
            .flatten();
        assert_eq!(single, simple, "evaluate_flag_simple for {key}");
    }
    single
}

#[test]
fn explicit_person_condition_on_a_group_flag_runs_without_a_group_key() {
    for groups in groups_without_a_usable_key() {
        for (person_plan, expected) in [("pro", true), ("free", false)] {
            assert_eq!(
                evaluate(
                    definitions(vec![person_condition_on_company_flag()]),
                    "pro-plan",
                    &plan(person_plan),
                    &groups
                ),
                Some(FlagValue::Boolean(expected)),
                "plan {person_plan}, groups {groups:?}"
            );
        }
    }
}

#[test]
fn dependency_on_a_group_flag_runs_its_explicit_person_condition() {
    for groups in groups_without_a_usable_key() {
        for (person_plan, expected) in [("pro", true), ("free", false)] {
            assert_eq!(
                evaluate(
                    definitions(vec![
                        person_condition_on_company_flag(),
                        depends_on("pro-plan")
                    ]),
                    "dependent",
                    &plan(person_plan),
                    &groups
                ),
                Some(FlagValue::Boolean(expected)),
                "plan {person_plan}, groups {groups:?}"
            );
        }
    }
}

#[test]
fn skipped_group_condition_leaves_the_person_condition_to_decide() {
    for groups in groups_without_a_usable_key() {
        for (person_plan, expected) in [("pro", true), ("free", false)] {
            assert_eq!(
                evaluate(
                    definitions(vec![mixed_company_flag()]),
                    "mixed",
                    &plan(person_plan),
                    &groups
                ),
                Some(FlagValue::Boolean(expected)),
                "plan {person_plan}, groups {groups:?}"
            );
        }
    }
}

#[test]
fn group_flag_without_a_group_key_is_false() {
    for holdout in [None, Some(json!({"id": 7, "exclusion_percentage": 0}))] {
        for groups in groups_without_a_usable_key() {
            assert_eq!(
                evaluate(
                    definitions(vec![company_flag(holdout.clone())]),
                    "company-flag",
                    &plan("pro"),
                    &groups
                ),
                Some(FlagValue::Boolean(false)),
                "holdout {holdout:?}, groups {groups:?}"
            );
        }
    }
}

#[test]
fn holdout_on_a_group_flag_without_a_group_key_is_inconclusive() {
    let holdout = json!({"id": 7, "exclusion_percentage": 50});
    for groups in groups_without_a_usable_key() {
        assert_eq!(
            evaluate(
                definitions(vec![company_flag(Some(holdout.clone()))]),
                "company-flag",
                &plan("pro"),
                &groups
            ),
            None,
            "groups {groups:?}"
        );
        assert_eq!(
            evaluate(
                definitions(vec![
                    company_flag(Some(holdout.clone())),
                    depends_on("company-flag")
                ]),
                "dependent",
                &plan("pro"),
                &groups
            ),
            None,
            "dependent, groups {groups:?}"
        );
    }
}

#[test]
fn holdout_on_a_group_flag_with_a_group_key_is_unchanged() {
    let groups = HashMap::from([("company".to_string(), "acme".to_string())]);
    let evaluate = |holdout| {
        evaluate(
            definitions(vec![company_flag(holdout)]),
            "company-flag",
            &plan("pro"),
            &groups,
        )
    };
    assert_eq!(
        evaluate(Some(json!({"id": 7, "exclusion_percentage": 50}))),
        evaluate(None)
    );
}

#[test]
fn definitions_keep_explicit_person_conditions_and_holdouts_through_serialization() {
    let flags = vec![
        person_condition_on_company_flag(),
        company_flag(Some(json!({"id": 7, "exclusion_percentage": 50}))),
    ];
    let response: LocalEvaluationResponse =
        serde_json::from_value(definitions(flags.clone())).unwrap();
    let sent = serde_json::to_value(&response).unwrap();
    assert_eq!(
        sent["flags"][0]["filters"]["groups"][0]["aggregation_group_type_index"],
        Value::Null,
        "an explicit person condition stays explicit"
    );
    assert_eq!(
        sent["flags"][1]["filters"]["holdout"],
        flags[1]["filters"]["holdout"]
    );
    assert_eq!(
        sent["flags"][1]["filters"]["groups"][0].get("aggregation_group_type_index"),
        None,
        "an absent condition aggregation stays absent"
    );

    let evaluator = evaluator(sent);
    for groups in groups_without_a_usable_key() {
        let group_properties = HashMap::new();
        assert_eq!(
            evaluator
                .evaluate_flag(
                    "pro-plan",
                    "user-1",
                    &plan("pro"),
                    &groups,
                    &group_properties
                )
                .unwrap(),
            Some(FlagValue::Boolean(true))
        );
        assert!(evaluator
            .evaluate_flag(
                "company-flag",
                "user-1",
                &plan("pro"),
                &groups,
                &group_properties
            )
            .is_err());
    }
}

#[cfg(feature = "async-client")]
mod client {
    use super::*;
    use httpmock::prelude::*;
    use posthog::{ClientOptionsBuilder, EvaluateFlagsOptions};

    async fn client(server: &MockServer, flags: Vec<Value>) -> posthog::Client {
        server.mock(|when, then| {
            when.method(GET).path("/flags/definitions/");
            then.status(200).json_body(definitions(flags.clone()));
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

    fn options(
        flag_keys: &[&str],
        person_plan: &str,
        groups: HashMap<String, String>,
        only_evaluate_locally: bool,
    ) -> EvaluateFlagsOptions {
        let mut options = EvaluateFlagsOptions::default();
        options.flag_keys = Some(flag_keys.iter().map(|key| key.to_string()).collect());
        options.person_properties = Some(plan(person_plan));
        options.groups = Some(groups);
        options.only_evaluate_locally = only_evaluate_locally;
        options
    }

    #[tokio::test]
    async fn explicit_person_conditions_resolve_locally_without_a_request() {
        let server = MockServer::start_async().await;
        let remote = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(500);
        });
        let client = client(
            &server,
            vec![
                person_condition_on_company_flag(),
                mixed_company_flag(),
                company_flag(None),
                depends_on("pro-plan"),
            ],
        )
        .await;
        let keys = ["pro-plan", "mixed", "company-flag", "dependent"];

        for groups in groups_without_a_usable_key() {
            for (person_plan, expected) in [("pro", true), ("free", false)] {
                for only_evaluate_locally in [false, true] {
                    let flags = client
                        .evaluate_flags(
                            "user-1",
                            options(&keys, person_plan, groups.clone(), only_evaluate_locally),
                        )
                        .await
                        .unwrap();
                    for key in ["pro-plan", "mixed", "dependent"] {
                        assert_eq!(
                            flags.get_flag(key),
                            Some(FlagValue::Boolean(expected)),
                            "{key}, plan {person_plan}, groups {groups:?}"
                        );
                    }
                    assert_eq!(
                        flags.get_flag("company-flag"),
                        Some(FlagValue::Boolean(false))
                    );
                }
            }
        }
        remote.assert_calls(0);
    }

    #[tokio::test]
    async fn holdout_without_a_group_key_falls_back_to_the_server() {
        let server = MockServer::start_async().await;
        let remote = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(json!({
                "flags": {"company-flag": {
                    "key": "company-flag", "enabled": true, "variant": "holdout-7",
                    "metadata": {"id": 1, "version": 1}
                }},
                "errorsWhileComputingFlags": false,
                "requestId": "request-1"
            }));
        });
        let client = client(
            &server,
            vec![company_flag(Some(
                json!({"id": 7, "exclusion_percentage": 50}),
            ))],
        )
        .await;

        let local = client
            .evaluate_flags(
                "user-1",
                options(&["company-flag"], "pro", HashMap::new(), true),
            )
            .await
            .unwrap();
        assert_eq!(local.get_flag("company-flag"), None);
        remote.assert_calls(0);

        let flags = client
            .evaluate_flags(
                "user-1",
                options(&["company-flag"], "pro", HashMap::new(), false),
            )
            .await
            .unwrap();
        assert_eq!(
            flags.get_flag("company-flag"),
            Some(FlagValue::String("holdout-7".to_string()))
        );
        remote.assert_calls(1);
    }
}

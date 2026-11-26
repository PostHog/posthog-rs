use posthog::{FlagCache, FlagValue, LocalEvaluationResponse, LocalEvaluator};
use serde_json::{json, Value};
use std::collections::HashMap;

fn evaluator(parent: Value, dependency: Value) -> LocalEvaluator {
    let mut flags = vec![
        json!({"key": "child", "active": true, "filters": {"groups": [
            {"properties": [dependency], "rollout_percentage": 100}
        ]}}),
    ];
    if !parent.is_null() {
        flags.push(parent);
    }
    let response: LocalEvaluationResponse =
        serde_json::from_value(json!({"flags": flags})).unwrap();
    let cache = FlagCache::new();
    cache.update(response);
    LocalEvaluator::new(cache)
}

fn wire(key: &str, value: Value, chain: Value) -> Value {
    json!({"key": key, "type": "flag", "operator": "flag_evaluates_to", "value": value, "dependency_chain": chain})
}

fn plan_parent() -> Value {
    json!({"key": "parent", "active": true, "filters": {"groups": [{"properties": [
        {"key": "plan", "value": "pro", "operator": "exact", "type": "person"}
    ], "rollout_percentage": 100}]}})
}

fn variant_parent(variant: &str) -> Value {
    json!({"key": "parent", "active": true, "filters": {
        "groups": [{"properties": [], "rollout_percentage": 100, "variant": variant}],
        "multivariate": {"variants": [
            {"key": "test", "rollout_percentage": 50}, {"key": "control", "rollout_percentage": 50}
        ]}
    }})
}

fn child(evaluator: &LocalEvaluator, properties: Value) -> Result<Option<FlagValue>, String> {
    let properties: HashMap<String, Value> = serde_json::from_value(properties).unwrap();
    evaluator
        .evaluate_flag(
            "child",
            "user",
            &properties,
            &HashMap::new(),
            &HashMap::new(),
        )
        .map_err(|e| e.to_string())
}

const T: Option<FlagValue> = Some(FlagValue::Boolean(true));
const F: Option<FlagValue> = Some(FlagValue::Boolean(false));

// Spec: "Flag dependency filters compare evaluated values".
#[test]
fn server_shaped_dependency_compares_evaluated_values() {
    let cases = [
        ("test", json!(true), T),
        ("test", json!("test"), T),
        ("test", json!("Test"), F),
        ("control", json!("test"), F),
    ];
    for (variant, expected, result) in cases {
        let e = evaluator(
            variant_parent(variant),
            wire("parent", expected.clone(), json!(["parent"])),
        );
        assert_eq!(child(&e, json!({})), Ok(result), "{variant} vs {expected}");
    }
    let parent = json!({"key": "parent", "active": true, "filters": {"groups": [{"properties": [], "rollout_percentage": 100}]}});
    for (expected, result) in [(json!(true), T), (json!(false), F)] {
        let e = evaluator(
            parent.clone(),
            wire("parent", expected.clone(), json!(["parent"])),
        );
        assert_eq!(child(&e, json!({})), Ok(result), "true vs {expected}");
    }
}

// Spec: the dependency evaluates with the same person context.
#[test]
fn server_shaped_dependency_sees_person_properties() {
    let e = evaluator(
        plan_parent(),
        wire("parent", json!(true), json!(["parent"])),
    );
    assert_eq!(child(&e, json!({"plan": "pro"})), Ok(T));
    assert_eq!(child(&e, json!({"plan": "free"})), Ok(F));
}

// Spec: "An inactive dependency resolves as false".
#[test]
fn inactive_dependency_resolves_as_false() {
    let parent = json!({"key": "parent", "active": false, "filters": {"groups": []}});
    let e = evaluator(
        parent.clone(),
        wire("parent", json!(false), json!(["parent"])),
    );
    assert_eq!(child(&e, json!({})), Ok(T));
    let e = evaluator(parent, wire("parent", json!(true), json!(["parent"])));
    assert_eq!(child(&e, json!({})), Ok(F));
}

// Spec: a missing definition or a cycle is inconclusive.
#[test]
fn missing_or_cyclic_dependency_is_inconclusive() {
    let e = evaluator(Value::Null, wire("parent", json!(true), json!([])));
    assert!(child(&e, json!({})).is_err());

    let cyclic = json!({"key": "parent", "active": true, "filters": {"groups": [{"properties": [
        wire("child", json!(true), json!([]))
    ], "rollout_percentage": 100}]}});
    let e = evaluator(cyclic, wire("parent", json!(true), json!([])));
    assert!(child(&e, json!({})).is_err());
}

// The flags service rejects any operator other than `flag_evaluates_to`.
#[test]
fn another_operator_never_matches() {
    let parent = json!({"key": "parent", "active": true, "filters": {"groups": [{"properties": [], "rollout_percentage": 100}]}});
    for operator in ["exact", "is_not"] {
        let filter = json!({"key": "parent", "type": "flag", "operator": operator, "value": true, "dependency_chain": ["parent"]});
        let e = evaluator(parent.clone(), filter);
        assert_eq!(child(&e, json!({})), Ok(F), "{operator}");
    }
    let mut no_value = wire("parent", json!(true), json!(["parent"]));
    no_value.as_object_mut().unwrap().remove("value");
    assert_eq!(child(&evaluator(parent, no_value), json!({})), Ok(F));
}

// `/flags/definitions` never sends `$feature/<key>`: such a filter is an
// ordinary person property, as on the server.
#[test]
fn feature_prefix_key_is_a_person_property() {
    let filter = json!({"key": "$feature/parent", "value": true, "operator": "exact"});
    let e = evaluator(plan_parent(), filter);
    assert_eq!(child(&e, json!({"$feature/parent": true})), Ok(T));
    assert_eq!(
        child(&e, json!({"$feature/parent": false, "plan": "pro"})),
        Ok(F)
    );
    assert!(child(&e, json!({"plan": "pro"})).is_err());
}

// The flags service doesn't resolve a dependency inside a cohort: a
// `flag_evaluates_to` leaf never matches, and any other operator matches the
// leaf's key against person properties.
#[test]
fn flag_leaf_in_a_cohort_follows_the_flags_service() {
    let cases = [
        (
            wire("parent", json!(true), json!(["parent"])),
            json!({"plan": "pro"}),
            F,
        ),
        (
            wire("parent", json!(false), json!(["parent"])),
            json!({"plan": "free"}),
            F,
        ),
        (
            json!({"key": "parent", "type": "flag", "operator": "flag_evaluates_to", "value": true, "negation": true}),
            json!({"plan": "pro"}),
            T,
        ),
        (
            json!({"key": "parent", "type": "flag", "operator": "exact", "value": true}),
            json!({"parent": true}),
            T,
        ),
        (
            json!({"key": "parent", "type": "flag", "operator": "exact", "value": true}),
            json!({"parent": false}),
            F,
        ),
        (
            json!({"key": "parent", "type": "flag", "operator": "exact"}),
            json!({"parent": null}),
            F,
        ),
    ];
    for (leaf, properties, expected) in cases {
        let response: LocalEvaluationResponse = serde_json::from_value(json!({
            "flags": [
                plan_parent(),
                {"key": "child", "active": true, "filters": {"groups": [{"properties": [
                    {"key": "id", "type": "cohort", "value": 1}
                ], "rollout_percentage": 100}]}}
            ],
            "cohorts": {"1": {"type": "OR", "values": [{"type": "AND", "values": [leaf.clone()]}]}}
        }))
        .unwrap();
        let cache = FlagCache::new();
        cache.update(response);
        let e = LocalEvaluator::new(cache);
        assert_eq!(child(&e, properties), Ok(expected), "{leaf}");
    }
    let response: LocalEvaluationResponse = serde_json::from_value(json!({
        "flags": [{"key": "child", "active": true, "filters": {"groups": [{"properties": [
            {"key": "id", "type": "cohort", "value": 1}
        ], "rollout_percentage": 100}]}}],
        "cohorts": {"1": {"type": "AND", "values": [
            {"key": "parent", "type": "flag", "operator": "exact", "value": true}
        ]}}
    }))
    .unwrap();
    let cache = FlagCache::new();
    cache.update(response);
    assert!(child(&LocalEvaluator::new(cache), json!({})).is_err());
}

// Spec: "A dependency is evaluated with its own aggregation".
#[test]
fn dependency_uses_its_own_aggregation() {
    let response: LocalEvaluationResponse = serde_json::from_value(json!({
        "group_type_mapping": {"0": "organization"},
        "flags": [
            {"key": "org-parent", "active": true, "filters": {"aggregation_group_type_index": 0, "groups": [
                {"properties": [{"key": "region", "type": "group", "group_type_index": 0, "operator": "exact", "value": "us"}], "rollout_percentage": 100}
            ]}},
            {"key": "child", "active": true, "filters": {"groups": [{"properties": [
                wire("org-parent", json!(true), json!(["org-parent"]))
            ], "rollout_percentage": 100}]}}
        ]
    }))
    .unwrap();
    let cache = FlagCache::new();
    cache.update(response);
    let e = LocalEvaluator::new(cache);
    let groups = HashMap::from([("organization".to_string(), "acme".to_string())]);
    for (person_region, group_region, expected) in [("us", "eu", F), ("eu", "us", T)] {
        let person = HashMap::from([("region".to_string(), json!(person_region))]);
        let group = HashMap::from([(
            "organization".to_string(),
            HashMap::from([("region".to_string(), json!(group_region))]),
        )]);
        assert_eq!(
            e.evaluate_flag("child", "user", &person, &groups, &group)
                .ok(),
            Some(expected)
        );
    }
    // Without the organization group, the parent's only condition is skipped.
    assert_eq!(
        e.evaluate_flag(
            "child",
            "user",
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new()
        )
        .ok(),
        Some(F)
    );
}

// A filter that can't match is decided without evaluating the referenced flag,
// which here would be inconclusive without the "plan" property.
#[test]
fn a_filter_that_cannot_match_skips_the_referenced_flag() {
    let mut no_value = wire("parent", json!(true), json!(["parent"]));
    no_value.as_object_mut().unwrap().remove("value");
    for filter in [no_value, wire("parent", json!(1), json!(["parent"]))] {
        assert_eq!(
            child(&evaluator(plan_parent(), filter.clone()), json!({})),
            Ok(F),
            "{filter}"
        );
    }
}

fn chain(len: usize, filters_per_flag: usize) -> LocalEvaluator {
    let mut flags = vec![
        json!({"key": "flag-0", "active": true, "filters": {"groups": [{"properties": [], "rollout_percentage": 100}]}}),
    ];
    for i in 1..=len {
        let previous = format!("flag-{}", i - 1);
        let filters: Vec<Value> = (0..filters_per_flag)
            .map(|_| wire(&previous, json!(true), json!([previous])))
            .collect();
        flags.push(
            json!({"key": format!("flag-{i}"), "active": true, "filters": {"groups": [
                {"properties": filters, "rollout_percentage": 100}
            ]}}),
        );
    }
    let response: LocalEvaluationResponse =
        serde_json::from_value(json!({"flags": flags})).unwrap();
    let cache = FlagCache::new();
    cache.update(response);
    LocalEvaluator::new(cache)
}

fn evaluate_last(e: &LocalEvaluator, len: usize) -> Result<Option<FlagValue>, String> {
    e.evaluate_flag(
        &format!("flag-{len}"),
        "user",
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    )
    .map_err(|e| e.to_string())
}

#[test]
fn a_long_dependency_chain_is_inconclusive_instead_of_overflowing() {
    assert_eq!(evaluate_last(&chain(100, 1), 100), Ok(T));
    assert!(evaluate_last(&chain(5000, 1), 5000).is_err());
}

// Each flag is evaluated once per call, so filters sharing a prerequisite don't
// multiply the work at every level.
#[test]
fn a_shared_prerequisite_is_evaluated_once() {
    assert_eq!(evaluate_last(&chain(60, 4), 60), Ok(T));
}

// A depth-limit failure on one path must not decide a later, shorter path.
#[test]
fn a_failed_long_path_does_not_decide_a_direct_dependency() {
    let len = 150;
    let mut flags = vec![
        json!({"key": "flag-0", "active": true, "filters": {"groups": [{"properties": [], "rollout_percentage": 100}]}}),
    ];
    for i in 1..=len {
        let previous = format!("flag-{}", i - 1);
        flags.push(json!({"key": format!("flag-{i}"), "active": true, "filters": {"groups": [
            {"properties": [wire(&previous, json!(true), json!([previous]))], "rollout_percentage": 100}
        ]}}));
    }
    // The first condition walks the long chain and hits the depth limit at
    // flag-50. The second depends on flag-50 directly, at a depth that is fine.
    flags.push(json!({"key": "child", "active": true, "filters": {"groups": [
        {"properties": [wire(&format!("flag-{len}"), json!(true), json!([]))], "rollout_percentage": 100},
        {"properties": [wire("flag-50", json!(true), json!(["flag-50"]))], "rollout_percentage": 100}
    ]}}));
    let response: LocalEvaluationResponse =
        serde_json::from_value(json!({"flags": flags})).unwrap();
    let cache = FlagCache::new();
    cache.update(response);
    assert_eq!(child(&LocalEvaluator::new(cache), json!({})), Ok(T));
}

// A cycle reached through branching conditions makes callers on that path skip
// the cache, so the work is capped instead of growing with every level.
#[test]
fn a_cycle_behind_branching_conditions_is_inconclusive_quickly() {
    let len = 30;
    let mut flags = Vec::new();
    for i in 0..len {
        let next = format!("flag-{}", i + 1);
        let condition = json!({"properties": [wire(&next, json!(true), json!([next]))], "rollout_percentage": 100});
        flags.push(json!({"key": format!("flag-{i}"), "active": true, "filters": {"groups": [condition.clone(), condition]}}));
    }
    let last = format!("flag-{len}");
    flags.push(json!({"key": last, "active": true, "filters": {"groups": [
        {"properties": [wire(&last, json!(true), json!([]))], "rollout_percentage": 100}
    ]}}));
    let response: LocalEvaluationResponse =
        serde_json::from_value(json!({"flags": flags})).unwrap();
    let cache = FlagCache::new();
    cache.update(response);
    let e = LocalEvaluator::new(cache);
    let started = std::time::Instant::now();
    assert!(e
        .evaluate_flag(
            "flag-0",
            "user",
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new()
        )
        .is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn only_a_flag_filter_may_omit_its_value() {
    let parse = |filter: Value| serde_json::from_value::<posthog::Property>(filter).is_ok();
    assert!(parse(
        json!({"key": "parent", "type": "flag", "operator": "flag_evaluates_to"})
    ));
    assert!(!parse(
        json!({"key": "plan", "type": "person", "operator": "flag_evaluates_to"})
    ));
    assert!(!parse(json!({"key": "plan", "operator": "exact"})));
}

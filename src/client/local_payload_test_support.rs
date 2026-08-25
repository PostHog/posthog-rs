//! Shared fixtures for the local-evaluation payload tests in `async_client`
//! and `blocking`. Those two modules are mutually exclusive under the
//! `async-client` feature, so this lives outside both to avoid two copies
//! drifting out of sync.

use std::collections::HashMap;

use serde_json::json;

use crate::feature_flag_evaluations::FeatureFlagEvaluations;
use crate::feature_flags::{
    FeatureFlag, FeatureFlagCondition, FeatureFlagFilters, FlagValue, MultivariateFilter,
    MultivariateVariant,
};
use crate::local_evaluation::LocalEvaluationResponse;

fn flag(
    key: &str,
    rollout_percentage: f64,
    multivariate: Option<MultivariateFilter>,
    payloads: &[(&str, serde_json::Value)],
) -> FeatureFlag {
    FeatureFlag {
        key: key.into(),
        active: true,
        has_experiment: None,
        filters: FeatureFlagFilters {
            groups: vec![FeatureFlagCondition {
                properties: vec![],
                rollout_percentage: Some(rollout_percentage),
                variant: None,
                aggregation_group_type_index: None,
            }],
            multivariate,
            payloads: payloads
                .iter()
                .map(|(key, payload)| ((*key).to_string(), payload.clone()))
                .collect(),
            aggregation_group_type_index: None,
            early_exit: false,
        },
    }
}

/// Definitions covering every shape a payload can take in the manifest, all
/// evaluable without person properties.
pub(super) fn payload_definitions() -> LocalEvaluationResponse {
    let variants = MultivariateFilter {
        variants: vec![
            MultivariateVariant {
                key: "test".into(),
                rollout_percentage: 100.0,
            },
            MultivariateVariant {
                key: "control".into(),
                rollout_percentage: 0.0,
            },
        ],
    };

    LocalEvaluationResponse {
        flags: vec![
            // The definitions endpoint JSON-encodes payloads, so the common
            // case is an object stored as a string.
            flag(
                "json-string-payload",
                100.0,
                None,
                &[("true", json!("{\"color\": \"blue\"}"))],
            ),
            // Definitions written before payloads were normalised server-side
            // can still carry an already-parsed value.
            flag(
                "parsed-payload",
                100.0,
                None,
                &[("true", json!({"color": "blue"}))],
            ),
            // A string payload is stored double-encoded.
            flag(
                "quoted-string-payload",
                100.0,
                None,
                &[("true", json!("\"just text\""))],
            ),
            // Not valid JSON: the raw string survives rather than erroring.
            flag(
                "undecodable-payload",
                100.0,
                None,
                &[("true", json!("not json"))],
            ),
            // Keyed by variant, so the wrong variant's payload must not leak.
            flag(
                "variant-payload",
                100.0,
                Some(variants),
                &[
                    ("test", json!("{\"tier\": 2}")),
                    ("control", json!("{\"tier\": 1}")),
                ],
            ),
            flag("no-payload", 100.0, None, &[]),
            // Evaluates false, so `/flags` would report no payload either.
            flag(
                "disabled-with-payload",
                0.0,
                None,
                &[("true", json!("{\"unreachable\": true}"))],
            ),
        ],
        group_type_mapping: HashMap::new(),
        cohorts: HashMap::new(),
        minimal_flag_called_events: false,
    }
}

pub(super) fn assert_payloads_match_remote_shape(snapshot: &FeatureFlagEvaluations) {
    assert_eq!(
        snapshot.get_flag_payload("json-string-payload"),
        Some(json!({"color": "blue"}))
    );
    assert_eq!(
        snapshot.get_flag_payload("parsed-payload"),
        Some(json!({"color": "blue"}))
    );
    assert_eq!(
        snapshot.get_flag_payload("quoted-string-payload"),
        Some(json!("just text"))
    );
    assert_eq!(
        snapshot.get_flag_payload("undecodable-payload"),
        Some(json!("not json"))
    );
}

pub(super) fn assert_payload_is_keyed_by_matched_variant(snapshot: &FeatureFlagEvaluations) {
    assert_eq!(
        snapshot.get_flag("variant-payload"),
        Some(FlagValue::String("test".to_string()))
    );
    assert_eq!(
        snapshot.get_flag_payload("variant-payload"),
        Some(json!({"tier": 2}))
    );
}

pub(super) fn assert_payload_is_absent_without_match(snapshot: &FeatureFlagEvaluations) {
    assert_eq!(snapshot.get_flag_payload("no-payload"), None);
    assert_eq!(snapshot.get_flag_payload("not-a-flag"), None);

    // A missing key also yields `None`, so pin the flag down first:
    // it was evaluated, it evaluated false, and its "true" payload
    // stayed behind.
    assert_eq!(
        snapshot.get_flag("disabled-with-payload"),
        Some(FlagValue::Boolean(false))
    );
    assert_eq!(snapshot.get_flag_payload("disabled-with-payload"), None);
}

//! Shared fixtures for the local-evaluation payload tests in `async_client`
//! and `blocking`. Those two modules are mutually exclusive under the
//! `async-client` feature, so this lives outside both to avoid two copies
//! drifting out of sync.

use std::collections::HashMap;

use serde_json::json;

use crate::feature_flags::{
    FeatureFlag, FeatureFlagCondition, FeatureFlagFilters, MultivariateFilter, MultivariateVariant,
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

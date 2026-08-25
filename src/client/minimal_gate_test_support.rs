//! Shared fixtures for the minimization-gate tests in `async_client` and
//! `blocking`. Those two modules are mutually exclusive under the
//! `async-client` feature, so this lives outside both to avoid two copies
//! drifting out of sync.
#![cfg(test)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::feature_flag_evaluations::{
    FeatureFlagEvaluations, FeatureFlagEvaluationsHost, FlagCalledEventParams,
};
use crate::feature_flags::{FeatureFlag, FeatureFlagCondition, FeatureFlagFilters};
use crate::local_evaluation::{FlagCache, LocalEvaluationResponse};

#[derive(Default)]
pub(super) struct RecordingHost {
    pub(super) captured: Mutex<Vec<FlagCalledEventParams>>,
}

impl FeatureFlagEvaluationsHost for RecordingHost {
    fn capture_flag_called_event_if_needed(&self, params: FlagCalledEventParams) {
        self.captured.lock().unwrap().push(params);
    }
    fn log_warning(&self, _message: &str) {}
}

/// A flag that evaluates locally to `true` (active, 100% rollout, no property
/// filters), carrying the given experiment signal.
fn gated_flag(has_experiment: Option<bool>) -> FeatureFlag {
    FeatureFlag {
        key: "gated".into(),
        active: true,
        has_experiment,
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
    }
}

pub(super) fn definitions(has_experiment: Option<bool>, gate: bool) -> LocalEvaluationResponse {
    LocalEvaluationResponse {
        flags: vec![gated_flag(has_experiment)],
        group_type_mapping: HashMap::new(),
        cohorts: HashMap::new(),
        minimal_flag_called_events: gate,
    }
}

pub(super) struct GateTestFixture {
    pub(super) cache: FlagCache,
    pub(super) host: Arc<RecordingHost>,
}

pub(super) fn gate_test_fixture(has_experiment: Option<bool>, gate: bool) -> GateTestFixture {
    let cache = FlagCache::new();
    cache.update(definitions(has_experiment, gate));
    GateTestFixture {
        cache,
        host: Arc::new(RecordingHost::default()),
    }
}

pub(super) fn assert_gate_was_pinned(
    snapshot: &FeatureFlagEvaluations,
    host: &RecordingHost,
    expected_minimal: bool,
) {
    assert!(snapshot.is_enabled("gated"));
    let captured = host.captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(
        captured[0].minimal, expected_minimal,
        "event must reflect the gate pinned at evaluation, not the mutated cache"
    );
}

pub(super) fn assert_has_experiment_was_threaded(
    snapshot: &FeatureFlagEvaluations,
    host: &RecordingHost,
) {
    assert!(snapshot.is_enabled("gated"));
    let captured = host.captured.lock().unwrap();
    assert_eq!(
        captured[0].properties.get("$feature_flag_has_experiment"),
        Some(&serde_json::json!(false))
    );
    assert!(captured[0].minimal);
}

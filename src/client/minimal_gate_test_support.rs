//! Shared fixtures for the minimization-gate tests in `async_client` and
//! `blocking`. Those two modules are mutually exclusive under the
//! `async-client` feature, so this lives outside both to avoid two copies
//! drifting out of sync.
#![cfg(test)]

use std::collections::HashMap;
use std::sync::Mutex;

use crate::feature_flag_evaluations::{FeatureFlagEvaluationsHost, FlagCalledEventParams};
use crate::feature_flags::{FeatureFlag, FeatureFlagCondition, FeatureFlagFilters};
use crate::local_evaluation::LocalEvaluationResponse;

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

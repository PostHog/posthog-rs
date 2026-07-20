//! Snapshot-based feature flag evaluations.
//!
//! [`FeatureFlagEvaluations`] is the result of [`Client::evaluate_flags`] — a
//! cache of evaluated flag values for a single `distinct_id` plus the rich
//! metadata returned by `/flags?v=2` (request id, evaluated-at timestamp, per-flag
//! id/version/reason/payload). Repeated `is_enabled`/`get_flag` calls on the same
//! snapshot are deduplicated client-side, so server-side feature gating no longer
//! costs an HTTP round-trip per branch.
//!
//! The companion [`Event::with_flags`](crate::Event::with_flags) builder attaches
//! the snapshot's flag state (`$feature/<key>` and `$active_feature_flags`) to a
//! capture event without making another `/flags` call.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::feature_flags::FlagValue;

/// One evaluated flag inside a [`FeatureFlagEvaluations`] snapshot.
///
/// Carries everything needed to emit a fully-detailed `$feature_flag_called`
/// event without a follow-up network call.
#[derive(Debug, Clone)]
pub(crate) struct EvaluatedFlagRecord {
    pub enabled: bool,
    pub variant: Option<String>,
    pub payload: Option<Value>,
    pub id: Option<u64>,
    pub version: Option<u32>,
    pub reason: Option<String>,
    pub locally_evaluated: bool,
    /// Server-reported experiment linkage for this flag. Tri-state: `Some(bool)`
    /// when reported, `None` when unknown. Drives `$feature_flag_has_experiment`
    /// and, with the gate below, event minimization.
    pub has_experiment: Option<bool>,
    /// The minimal-`$feature_flag_called` gate captured from the source that
    /// produced this record (the poller's local definitions or the remote
    /// `/flags` response). Pinned per record so the minimization decision uses
    /// the value tied to this flag's evaluation, never a later read of shared
    /// mutable client state.
    pub minimal_flag_called_events: bool,
}

/// Parameters dispatched to [`FeatureFlagEvaluationsHost::capture_flag_called_event_if_needed`]
/// each time a snapshot method records a flag access.
#[derive(Debug, Clone)]
pub(crate) struct FlagCalledEventParams {
    pub distinct_id: String,
    pub key: String,
    pub response: Option<FlagValue>,
    pub groups: HashMap<String, String>,
    pub disable_geoip: Option<bool>,
    pub properties: HashMap<String, Value>,
    /// Whether this event should be minimized to the strict property allowlist.
    /// Decided by [`FeatureFlagEvaluations::record_access`] from the flag's own
    /// pinned gate and experiment signal, then applied as the final capture step.
    pub minimal: bool,
}

/// Dependency-inverted host interface used by [`FeatureFlagEvaluations`] to
/// emit dedup-aware `$feature_flag_called` events. The client constructs one
/// of these once and shares it across all snapshots it produces.
pub(crate) trait FeatureFlagEvaluationsHost: Send + Sync {
    fn capture_flag_called_event_if_needed(&self, params: FlagCalledEventParams);
    fn log_warning(&self, message: &str);
}

/// Optional inputs for [`Client::evaluate_flags`](crate::Client::evaluate_flags).
#[derive(Default, Clone, Debug)]
pub struct EvaluateFlagsOptions {
    /// Group keys for group-targeted feature flags, keyed by group type (for
    /// example `{ "company": "company_123" }`). These groups are also
    /// attached to `$feature_flag_called` events emitted by the returned
    /// snapshot.
    pub groups: Option<HashMap<String, String>>,
    /// Person properties used by remote or local flag evaluation. Provide any
    /// properties referenced by release conditions when using local evaluation.
    pub person_properties: Option<HashMap<String, Value>>,
    /// Group properties used by group-targeted or mixed-targeting flags, keyed
    /// first by group type and then by property name.
    pub group_properties: Option<HashMap<String, HashMap<String, Value>>>,
    /// When `true`, skip the remote `/flags` request and return only locally
    /// evaluated results. If local evaluation is not configured, the snapshot is
    /// empty.
    pub only_evaluate_locally: bool,
    /// Per-call override for GeoIP behavior on `/flags` and
    /// `$feature_flag_called` requests. `None` uses the client-level setting.
    pub disable_geoip: Option<bool>,
    /// Optional list of flag keys. When provided, only these flags are
    /// evaluated — the underlying `/flags` request asks the server for just
    /// this subset, which makes the response smaller and the request cheaper.
    /// Use this when you only need a handful of flags out of many.
    ///
    /// Distinct from [`FeatureFlagEvaluations::only`]: `flag_keys` trims the
    /// network call, [`only`](FeatureFlagEvaluations::only) trims which flags
    /// get attached to a captured event after evaluation.
    pub flag_keys: Option<Vec<String>>,
}

/// A snapshot of evaluated feature flags for one `distinct_id`.
///
/// Returned by [`Client::evaluate_flags`](crate::Client::evaluate_flags). Reading
/// flags via [`is_enabled`] or [`get_flag`] both records the access (so it can be
/// later attached to a capture event) and emits a deduplicated
/// `$feature_flag_called` event. [`get_flag_payload`] is intentionally event-free.
///
/// [`is_enabled`]: FeatureFlagEvaluations::is_enabled
/// [`get_flag`]: FeatureFlagEvaluations::get_flag
/// [`get_flag_payload`]: FeatureFlagEvaluations::get_flag_payload
pub struct FeatureFlagEvaluations {
    host: Arc<dyn FeatureFlagEvaluationsHost>,
    distinct_id: String,
    flags: HashMap<String, EvaluatedFlagRecord>,
    groups: HashMap<String, String>,
    disable_geoip: Option<bool>,
    request_id: Option<String>,
    evaluated_at: Option<i64>,
    errors_while_computing: bool,
    quota_limited: bool,
    accessed: Mutex<HashSet<String>>,
}

impl FeatureFlagEvaluations {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        host: Arc<dyn FeatureFlagEvaluationsHost>,
        distinct_id: String,
        flags: HashMap<String, EvaluatedFlagRecord>,
        groups: HashMap<String, String>,
        disable_geoip: Option<bool>,
        request_id: Option<String>,
        evaluated_at: Option<i64>,
        errors_while_computing: bool,
        quota_limited: bool,
    ) -> Self {
        Self {
            host,
            distinct_id,
            flags,
            groups,
            disable_geoip,
            request_id,
            evaluated_at,
            errors_while_computing,
            quota_limited,
            accessed: Mutex::new(HashSet::new()),
        }
    }

    /// Construct an empty snapshot used when no `distinct_id` was resolvable.
    /// The empty `distinct_id` short-circuits event firing inside
    /// [`record_access`](Self::record_access).
    pub(crate) fn empty(host: Arc<dyn FeatureFlagEvaluationsHost>) -> Self {
        Self::new(
            host,
            String::new(),
            HashMap::new(),
            HashMap::new(),
            None,
            None,
            None,
            false,
            false,
        )
    }

    /// Whether `key` is enabled. Records the access and fires (deduplicated)
    /// `$feature_flag_called`.
    ///
    /// # Returns
    ///
    /// `true` for enabled boolean flags or matched multivariate variants, and
    /// `false` for disabled or missing flags.
    #[must_use]
    pub fn is_enabled(&self, key: &str) -> bool {
        self.record_access(key);
        self.flags.get(key).is_some_and(|f| f.enabled)
    }

    /// Look up the value of `key`.
    ///
    /// # Returns
    ///
    /// - `None` when the flag is not in the snapshot,
    /// - `Some(FlagValue::Boolean(false))` when disabled,
    /// - `Some(FlagValue::String(variant))` for a multivariate match,
    /// - `Some(FlagValue::Boolean(true))` when enabled with no variant.
    ///
    /// Records the access and fires (deduplicated) `$feature_flag_called`.
    #[must_use]
    pub fn get_flag(&self, key: &str) -> Option<FlagValue> {
        self.record_access(key);
        let flag = self.flags.get(key)?;
        Some(flag_value_for(flag))
    }

    /// Return the JSON payload associated with `key`, if any.
    ///
    /// # Remarks
    ///
    /// This call does **not** count as an access and does **not** fire any
    /// event, matching the behavior documented for server-side SDKs.
    #[must_use]
    pub fn get_flag_payload(&self, key: &str) -> Option<Value> {
        self.flags.get(key).and_then(|f| f.payload.clone())
    }

    /// All flag keys present in this snapshot.
    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        self.flags.keys().cloned().collect()
    }

    /// A clone of the snapshot containing only flags whose values were read via
    /// [`is_enabled`](Self::is_enabled) or [`get_flag`](Self::get_flag) before
    /// this call.
    ///
    /// Order-dependent: if nothing has been accessed yet, the returned snapshot
    /// is empty. Pre-access the flags you want to attach before calling this.
    #[must_use]
    pub fn only_accessed(&self) -> Self {
        let accessed = self.snapshot_accessed();
        let filtered = self
            .flags
            .iter()
            .filter(|(k, _)| accessed.contains(k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        self.clone_with(filtered)
    }

    /// A clone of the snapshot containing only the listed `keys` (preserving
    /// records). Unknown keys are dropped and surfaced via a single warning.
    ///
    /// Use this before [`Event::with_flags`](crate::Event::with_flags) to limit
    /// the `$feature/<key>` properties attached to a captured event.
    #[must_use]
    pub fn only(&self, keys: &[&str]) -> Self {
        let mut filtered: HashMap<String, EvaluatedFlagRecord> = HashMap::new();
        let mut missing: Vec<&str> = Vec::new();
        for key in keys {
            match self.flags.get(*key) {
                Some(record) => {
                    filtered.insert((*key).to_string(), record.clone());
                }
                None => missing.push(*key),
            }
        }
        if !missing.is_empty() {
            self.host.log_warning(&format!(
                "FeatureFlagEvaluations::only() was called with flag keys that are not in the \
                 evaluation set and will be dropped: {}",
                missing.join(", ")
            ));
        }
        self.clone_with(filtered)
    }

    /// Build the property map for capture integration: `$feature/<key>` for
    /// every flag, plus a sorted `$active_feature_flags` list of enabled keys.
    pub(crate) fn event_properties(&self) -> HashMap<String, Value> {
        let mut props: HashMap<String, Value> = HashMap::with_capacity(self.flags.len() + 1);
        let mut active: Vec<String> = Vec::new();
        for (key, flag) in &self.flags {
            let value = flag_value_json(flag);
            props.insert(format!("$feature/{key}"), value);
            if flag.enabled {
                active.push(key.clone());
            }
        }
        if !active.is_empty() {
            active.sort();
            props.insert("$active_feature_flags".into(), json!(active));
        }
        props
    }

    fn snapshot_accessed(&self) -> HashSet<String> {
        match self.accessed.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }

    fn clone_with(&self, flags: HashMap<String, EvaluatedFlagRecord>) -> Self {
        Self {
            host: Arc::clone(&self.host),
            distinct_id: self.distinct_id.clone(),
            flags,
            groups: self.groups.clone(),
            disable_geoip: self.disable_geoip,
            request_id: self.request_id.clone(),
            evaluated_at: self.evaluated_at,
            errors_while_computing: self.errors_while_computing,
            quota_limited: self.quota_limited,
            accessed: Mutex::new(self.snapshot_accessed()),
        }
    }

    fn record_access(&self, key: &str) {
        if let Ok(mut accessed) = self.accessed.lock() {
            accessed.insert(key.to_string());
        }

        // Snapshots created without a resolvable distinct_id must never emit
        // `$feature_flag_called` — those events would land with an empty
        // distinct_id and pollute downstream analytics.
        if self.distinct_id.is_empty() {
            return;
        }

        let flag = self.flags.get(key);
        let response = flag.map(flag_value_for);
        let properties = self.build_called_event_properties(key, flag, &response);

        // Minimize iff the gate pinned on this flag's record is on AND the flag
        // is known to have no linked experiment. Any missing signal (gate off,
        // experiment unknown, experiment-linked, or missing flag) keeps the full
        // event shape. Read from the per-flag record, never from shared state.
        let minimal =
            flag.is_some_and(|f| f.minimal_flag_called_events && f.has_experiment == Some(false));

        self.host
            .capture_flag_called_event_if_needed(FlagCalledEventParams {
                distinct_id: self.distinct_id.clone(),
                key: key.to_string(),
                response,
                groups: self.groups.clone(),
                disable_geoip: self.disable_geoip,
                properties,
                minimal,
            });
    }

    fn build_called_event_properties(
        &self,
        key: &str,
        flag: Option<&EvaluatedFlagRecord>,
        response: &Option<FlagValue>,
    ) -> HashMap<String, Value> {
        let mut props: HashMap<String, Value> = HashMap::new();
        props.insert("$feature_flag".into(), json!(key));
        let response_json = match response {
            Some(v) => flag_value_to_json(v),
            None => Value::Null,
        };
        props.insert("$feature_flag_response".into(), response_json.clone());
        props.insert(format!("$feature/{key}"), response_json);

        let locally_evaluated = flag.is_some_and(|f| f.locally_evaluated);
        props.insert("locally_evaluated".into(), json!(locally_evaluated));

        // Record the server's experiment signal when known, so minimization's
        // impact can be segmented by it. Omitted entirely when unknown (never
        // fabricated as `false`).
        if let Some(has_experiment) = flag.and_then(|f| f.has_experiment) {
            props.insert("$feature_flag_has_experiment".into(), json!(has_experiment));
        }

        if let Some(flag) = flag {
            if let Some(payload) = &flag.payload {
                props.insert("$feature_flag_payload".into(), payload.clone());
            }
            if let Some(id) = flag.id {
                if id != 0 {
                    props.insert("$feature_flag_id".into(), json!(id));
                }
            }
            if let Some(version) = flag.version {
                if version != 0 {
                    props.insert("$feature_flag_version".into(), json!(version));
                }
            }
            if let Some(reason) = &flag.reason {
                if !reason.is_empty() {
                    props.insert("$feature_flag_reason".into(), json!(reason));
                }
            }
        }

        if let Some(request_id) = &self.request_id {
            props.insert("$feature_flag_request_id".into(), json!(request_id));
        }

        if !locally_evaluated {
            if let Some(evaluated_at) = self.evaluated_at {
                props.insert("$feature_flag_evaluated_at".into(), json!(evaluated_at));
            }
        }

        // Comma-joined `$feature_flag_error` matching the single-flag path's
        // granularity: response-level errors (errors-while-computing,
        // quota-limited) combine with per-flag errors (flag-missing) so
        // consumers can filter by type.
        let mut errors: Vec<&str> = Vec::new();
        if self.errors_while_computing {
            errors.push("errors_while_computing_flags");
        }
        if self.quota_limited {
            errors.push("quota_limited");
        }
        if flag.is_none() {
            errors.push("flag_missing");
        }
        if !errors.is_empty() {
            props.insert("$feature_flag_error".into(), json!(errors.join(",")));
        }

        props
    }
}

impl std::fmt::Debug for FeatureFlagEvaluations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeatureFlagEvaluations")
            .field("distinct_id", &self.distinct_id)
            .field("flags", &self.flags)
            .field("groups", &self.groups)
            .field("disable_geoip", &self.disable_geoip)
            .field("request_id", &self.request_id)
            .field("evaluated_at", &self.evaluated_at)
            .field("errors_while_computing", &self.errors_while_computing)
            .field("quota_limited", &self.quota_limited)
            .finish_non_exhaustive()
    }
}

fn flag_value_for(flag: &EvaluatedFlagRecord) -> FlagValue {
    if !flag.enabled {
        FlagValue::Boolean(false)
    } else if let Some(variant) = &flag.variant {
        FlagValue::String(variant.clone())
    } else {
        FlagValue::Boolean(true)
    }
}

fn flag_value_to_json(value: &FlagValue) -> Value {
    match value {
        FlagValue::Boolean(b) => json!(b),
        FlagValue::String(s) => json!(s),
    }
}

fn flag_value_json(flag: &EvaluatedFlagRecord) -> Value {
    flag_value_to_json(&flag_value_for(flag))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct RecordingHost {
        captured: StdMutex<Vec<FlagCalledEventParams>>,
        warnings: StdMutex<Vec<String>>,
    }

    impl FeatureFlagEvaluationsHost for RecordingHost {
        fn capture_flag_called_event_if_needed(&self, params: FlagCalledEventParams) {
            self.captured.lock().unwrap().push(params);
        }
        fn log_warning(&self, message: &str) {
            self.warnings.lock().unwrap().push(message.to_string());
        }
    }

    fn record(
        _key: &str,
        enabled: bool,
        variant: Option<&str>,
        locally_evaluated: bool,
    ) -> EvaluatedFlagRecord {
        EvaluatedFlagRecord {
            enabled,
            variant: variant.map(str::to_string),
            payload: None,
            id: Some(42),
            version: Some(7),
            reason: Some("condition match".into()),
            locally_evaluated,
            has_experiment: None,
            minimal_flag_called_events: false,
        }
    }

    fn build(
        host: Arc<dyn FeatureFlagEvaluationsHost>,
        distinct_id: &str,
    ) -> FeatureFlagEvaluations {
        let mut flags = HashMap::new();
        flags.insert("alpha".into(), record("alpha", true, Some("test"), false));
        flags.insert("beta".into(), record("beta", false, None, false));
        flags.insert("gamma".into(), record("gamma", true, None, true));
        FeatureFlagEvaluations::new(
            host,
            distinct_id.into(),
            flags,
            HashMap::new(),
            None,
            Some("req-1".into()),
            Some(1700000000),
            false,
            false,
        )
    }

    #[test]
    fn is_enabled_records_access_and_fires_event() {
        let host = Arc::new(RecordingHost::default());
        let snap = build(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            "u1",
        );
        assert!(snap.is_enabled("alpha"));
        let captured = host.captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].key, "alpha");
        let props = &captured[0].properties;
        assert_eq!(props.get("$feature_flag_id"), Some(&json!(42_u64)));
        assert_eq!(props.get("$feature_flag_version"), Some(&json!(7_u32)));
        assert_eq!(
            props.get("$feature_flag_reason"),
            Some(&json!("condition match"))
        );
        assert_eq!(props.get("$feature_flag_request_id"), Some(&json!("req-1")));
    }

    #[test]
    fn get_flag_payload_does_not_record_access_or_fire_event() {
        let host = Arc::new(RecordingHost::default());
        let snap = build(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            "u1",
        );
        assert!(snap.get_flag_payload("alpha").is_none());
        assert!(host.captured.lock().unwrap().is_empty());
    }

    #[test]
    fn empty_distinct_id_does_not_fire_events() {
        let host = Arc::new(RecordingHost::default());
        let snap =
            FeatureFlagEvaluations::empty(Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>);
        assert!(!snap.is_enabled("anything"));
        assert!(host.captured.lock().unwrap().is_empty());
    }

    #[test]
    fn locally_evaluated_event_omits_evaluated_at_and_carries_locally_evaluated_flag() {
        let host = Arc::new(RecordingHost::default());
        let mut flags = HashMap::new();
        flags.insert(
            "gamma".into(),
            EvaluatedFlagRecord {
                reason: Some("Evaluated locally".into()),
                ..record("gamma", true, None, true)
            },
        );
        let snap = FeatureFlagEvaluations::new(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            "u1".into(),
            flags,
            HashMap::new(),
            None,
            None,
            Some(1700000000),
            false,
            false,
        );
        let _ = snap.is_enabled("gamma");
        let captured = host.captured.lock().unwrap();
        let props = &captured[0].properties;
        assert_eq!(props.get("locally_evaluated"), Some(&json!(true)));
        assert_eq!(
            props.get("$feature_flag_reason"),
            Some(&json!("Evaluated locally"))
        );
        assert!(!props.contains_key("$feature_flag_evaluated_at"));
    }

    #[test]
    fn errors_while_computing_propagates_to_event() {
        let host = Arc::new(RecordingHost::default());
        let mut flags = HashMap::new();
        flags.insert("alpha".into(), record("alpha", true, Some("test"), false));
        let snap = FeatureFlagEvaluations::new(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            "u1".into(),
            flags,
            HashMap::new(),
            None,
            Some("req-1".into()),
            Some(1700000000),
            true,  // errors_while_computing
            false, // quota_limited
        );
        let _ = snap.is_enabled("alpha");
        let captured = host.captured.lock().unwrap();
        assert_eq!(
            captured[0].properties.get("$feature_flag_error"),
            Some(&json!("errors_while_computing_flags"))
        );
    }

    #[test]
    fn payload_can_be_set_directly() {
        let mut flags = HashMap::new();
        flags.insert(
            "alpha".into(),
            EvaluatedFlagRecord {
                payload: Some(json!({"hello": "world"})),
                ..record("alpha", true, None, false)
            },
        );
        let host = Arc::new(RecordingHost::default());
        let snap = FeatureFlagEvaluations::new(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            "u1".into(),
            flags,
            HashMap::new(),
            None,
            None,
            None,
            false,
            false,
        );
        assert_eq!(
            snap.get_flag_payload("alpha"),
            Some(json!({"hello": "world"}))
        );
    }

    #[test]
    fn quota_limited_combines_with_flag_missing_in_error_string() {
        let host = Arc::new(RecordingHost::default());
        let snap = FeatureFlagEvaluations::new(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            "u1".into(),
            HashMap::new(),
            HashMap::new(),
            None,
            None,
            None,
            false,
            true, // quota_limited
        );
        assert!(snap.get_flag("does-not-exist").is_none());
        let captured = host.captured.lock().unwrap();
        assert_eq!(
            captured[0].properties.get("$feature_flag_error"),
            Some(&json!("quota_limited,flag_missing"))
        );
    }

    #[test]
    fn missing_flag_records_flag_missing_error() {
        let host = Arc::new(RecordingHost::default());
        let snap = build(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            "u1",
        );
        assert!(snap.get_flag("does-not-exist").is_none());
        let captured = host.captured.lock().unwrap();
        assert_eq!(
            captured[0].properties.get("$feature_flag_error"),
            Some(&json!("flag_missing"))
        );
    }

    #[test]
    fn missing_flag_with_no_response_errors_emits_no_error_for_present_flag() {
        let host = Arc::new(RecordingHost::default());
        let snap = build(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            "u1",
        );
        assert!(snap.is_enabled("alpha"));
        let captured = host.captured.lock().unwrap();
        assert!(!captured[0].properties.contains_key("$feature_flag_error"));
    }

    #[test]
    fn only_accessed_filters_to_accessed_keys() {
        let host = Arc::new(RecordingHost::default());
        let snap = build(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            "u1",
        );
        let _ = snap.is_enabled("alpha");
        let filtered = snap.only_accessed();
        let mut keys = filtered.keys();
        keys.sort();
        assert_eq!(keys, vec!["alpha".to_string()]);
    }

    #[test]
    fn only_accessed_returns_empty_when_nothing_accessed() {
        let host = Arc::new(RecordingHost::default());
        let snap = build(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            "u1",
        );
        let filtered = snap.only_accessed();
        assert!(filtered.keys().is_empty());
        assert!(host.warnings.lock().unwrap().is_empty());
    }

    #[test]
    fn only_drops_unknown_keys_with_warning() {
        let host = Arc::new(RecordingHost::default());
        let snap = build(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            "u1",
        );
        let filtered = snap.only(&["alpha", "missing"]);
        assert_eq!(filtered.keys(), vec!["alpha".to_string()]);
        let warnings = host.warnings.lock().unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("missing"));
    }

    #[test]
    fn filtered_snapshots_do_not_back_propagate_access_to_parent() {
        let host = Arc::new(RecordingHost::default());
        let snap = build(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            "u1",
        );
        let _ = snap.is_enabled("alpha");
        let child = snap.only_accessed();
        let _ = child.is_enabled("alpha");
        // Parent's accessed set is still {"alpha"}, not affected by child reads.
        assert_eq!(snap.snapshot_accessed().len(), 1);
    }

    /// Build a snapshot holding a single flag with an explicit experiment
    /// signal and pinned minimization gate, mirroring what `evaluate_flags`
    /// produces per source.
    fn snapshot_with_flag(
        host: Arc<dyn FeatureFlagEvaluationsHost>,
        has_experiment: Option<bool>,
        minimal_flag_called_events: bool,
    ) -> FeatureFlagEvaluations {
        let mut flags = HashMap::new();
        flags.insert(
            "gated".into(),
            EvaluatedFlagRecord {
                has_experiment,
                minimal_flag_called_events,
                ..record("gated", true, None, false)
            },
        );
        FeatureFlagEvaluations::new(
            host,
            "u1".into(),
            flags,
            HashMap::new(),
            None,
            Some("req-1".into()),
            Some(1700000000),
            false,
            false,
        )
    }

    #[test]
    fn has_experiment_true_sets_property() {
        let host = Arc::new(RecordingHost::default());
        let snap = snapshot_with_flag(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            Some(true),
            false,
        );
        let _ = snap.is_enabled("gated");
        let captured = host.captured.lock().unwrap();
        assert_eq!(
            captured[0].properties.get("$feature_flag_has_experiment"),
            Some(&json!(true))
        );
    }

    #[test]
    fn has_experiment_false_sets_property() {
        let host = Arc::new(RecordingHost::default());
        let snap = snapshot_with_flag(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            Some(false),
            false,
        );
        let _ = snap.is_enabled("gated");
        let captured = host.captured.lock().unwrap();
        assert_eq!(
            captured[0].properties.get("$feature_flag_has_experiment"),
            Some(&json!(false))
        );
    }

    #[test]
    fn has_experiment_unknown_omits_property() {
        let host = Arc::new(RecordingHost::default());
        let snap = snapshot_with_flag(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            None,
            true,
        );
        let _ = snap.is_enabled("gated");
        let captured = host.captured.lock().unwrap();
        // Never fabricated as false when the server did not report it.
        assert!(!captured[0]
            .properties
            .contains_key("$feature_flag_has_experiment"));
    }

    #[test]
    fn minimizes_only_when_gate_on_and_no_experiment() {
        // (has_experiment, gate) -> expected `minimal`
        let cases = [
            (Some(false), true, true),   // gate on, no experiment -> minimize
            (Some(true), true, false),   // experiment-linked -> full
            (None, true, false),         // experiment unknown -> full
            (Some(false), false, false), // gate off -> full
        ];
        for (has_experiment, gate, expected) in cases {
            let host = Arc::new(RecordingHost::default());
            let snap = snapshot_with_flag(
                Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
                has_experiment,
                gate,
            );
            let _ = snap.is_enabled("gated");
            let captured = host.captured.lock().unwrap();
            assert_eq!(
                captured[0].minimal, expected,
                "has_experiment={:?} gate={} should minimal={}",
                has_experiment, gate, expected
            );
        }
    }

    #[test]
    fn missing_flag_is_never_minimized() {
        let host = Arc::new(RecordingHost::default());
        let snap = snapshot_with_flag(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            Some(false),
            true,
        );
        // A key absent from the snapshot has no pinned gate/experiment signal,
        // so it always keeps the full shape.
        let _ = snap.get_flag("not-present");
        let captured = host.captured.lock().unwrap();
        assert!(!captured[0].minimal);
    }

    #[test]
    fn gate_is_pinned_per_flag_not_shared_across_a_snapshot() {
        // Two flags in ONE snapshot with different pinned gates: a local flag
        // whose definitions had the gate on, and a remote flag whose /flags
        // response had it off. Each event must reflect its own source's gate.
        // Guards against collapsing the gate into a single shared snapshot field.
        let host = Arc::new(RecordingHost::default());
        let mut flags = HashMap::new();
        flags.insert(
            "local-gated".into(),
            EvaluatedFlagRecord {
                has_experiment: Some(false),
                minimal_flag_called_events: true,
                ..record("local-gated", true, None, true)
            },
        );
        flags.insert(
            "remote-ungated".into(),
            EvaluatedFlagRecord {
                has_experiment: Some(false),
                minimal_flag_called_events: false,
                ..record("remote-ungated", true, None, false)
            },
        );
        let snap = FeatureFlagEvaluations::new(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            "u1".into(),
            flags,
            HashMap::new(),
            None,
            Some("req-1".into()),
            Some(1700000000),
            false,
            false,
        );

        let _ = snap.is_enabled("local-gated");
        let _ = snap.is_enabled("remote-ungated");

        let captured = host.captured.lock().unwrap();
        let by_key: HashMap<&str, bool> = captured
            .iter()
            .map(|p| (p.key.as_str(), p.minimal))
            .collect();
        assert_eq!(by_key.get("local-gated"), Some(&true));
        assert_eq!(by_key.get("remote-ungated"), Some(&false));
    }

    #[test]
    fn event_properties_attaches_active_flags_sorted() {
        let host = Arc::new(RecordingHost::default());
        let snap = build(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            "u1",
        );
        let props = snap.event_properties();
        assert_eq!(props.get("$feature/alpha"), Some(&json!("test")));
        assert_eq!(props.get("$feature/beta"), Some(&json!(false)));
        assert_eq!(props.get("$feature/gamma"), Some(&json!(true)));
        let active = props.get("$active_feature_flags").unwrap();
        assert_eq!(active, &json!(["alpha", "gamma"]));
    }
}

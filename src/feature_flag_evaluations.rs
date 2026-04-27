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
pub struct EvaluatedFlagRecord {
    pub key: String,
    pub enabled: bool,
    pub variant: Option<String>,
    pub payload: Option<Value>,
    pub id: Option<u64>,
    pub version: Option<u32>,
    pub reason: Option<String>,
    pub locally_evaluated: bool,
}

/// Parameters dispatched to [`FeatureFlagEvaluationsHost::capture_flag_called_event_if_needed`]
/// each time a snapshot method records a flag access.
#[derive(Debug, Clone)]
pub struct FlagCalledEventParams {
    pub distinct_id: String,
    pub key: String,
    pub response: Option<FlagValue>,
    pub groups: HashMap<String, String>,
    pub disable_geoip: Option<bool>,
    pub properties: HashMap<String, Value>,
}

/// Dependency-inverted host interface used by [`FeatureFlagEvaluations`] to
/// emit dedup-aware `$feature_flag_called` events and surface filter-helper
/// warnings. The client constructs one of these once and shares it across all
/// snapshots it produces.
pub trait FeatureFlagEvaluationsHost: Send + Sync {
    fn capture_flag_called_event_if_needed(&self, params: FlagCalledEventParams);
    fn log_warning(&self, message: &str);
}

/// Optional inputs for [`Client::evaluate_flags`](crate::Client::evaluate_flags).
///
/// `flag_keys` scopes the underlying `/flags` request and is distinct from
/// [`FeatureFlagEvaluations::only`], which filters an in-memory snapshot.
#[derive(Default, Clone, Debug)]
pub struct EvaluateFlagsOptions {
    pub groups: Option<HashMap<String, String>>,
    pub person_properties: Option<HashMap<String, Value>>,
    pub group_properties: Option<HashMap<String, HashMap<String, Value>>>,
    pub only_evaluate_locally: bool,
    pub disable_geoip: Option<bool>,
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
    flag_definitions_loaded_at: Option<i64>,
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
        flag_definitions_loaded_at: Option<i64>,
    ) -> Self {
        Self {
            host,
            distinct_id,
            flags,
            groups,
            disable_geoip,
            request_id,
            evaluated_at,
            flag_definitions_loaded_at,
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
            None,
        )
    }

    /// Whether `key` is enabled. Records the access and fires (deduplicated)
    /// `$feature_flag_called`.
    #[must_use]
    pub fn is_enabled(&self, key: &str) -> bool {
        self.record_access(key);
        self.flags.get(key).is_some_and(|f| f.enabled)
    }

    /// Look up the value of `key`. Returns:
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

    /// Return the JSON payload associated with `key`, if any. This call does
    /// **not** count as an access and does **not** fire any event.
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
    /// If nothing has been accessed, logs a warning and falls back to returning
    /// a clone with all evaluated flags (so the captured event still carries
    /// flag context). Configure with
    /// [`ClientOptions::feature_flags_log_warnings`](crate::ClientOptionsBuilder)
    /// to silence the warning.
    #[must_use]
    pub fn only_accessed(&self) -> Self {
        let accessed = self.snapshot_accessed();
        if accessed.is_empty() {
            self.host.log_warning(
                "FeatureFlagEvaluations::only_accessed() was called before any flags were \
                 accessed — attaching all evaluated flags as a fallback. \
                 See https://posthog.com/docs/feature-flags/server-sdks for details.",
            );
            return self.clone_with(self.flags.clone());
        }
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
            flag_definitions_loaded_at: self.flag_definitions_loaded_at,
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

        self.host
            .capture_flag_called_event_if_needed(FlagCalledEventParams {
                distinct_id: self.distinct_id.clone(),
                key: key.to_string(),
                response,
                groups: self.groups.clone(),
                disable_geoip: self.disable_geoip,
                properties,
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
        } else {
            props.insert("$feature_flag_error".into(), json!("flag_missing"));
        }

        if locally_evaluated {
            if let Some(loaded_at) = self.flag_definitions_loaded_at {
                props.insert(
                    "$feature_flag_definitions_loaded_at".into(),
                    json!(loaded_at),
                );
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
            .field(
                "flag_definitions_loaded_at",
                &self.flag_definitions_loaded_at,
            )
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
        key: &str,
        enabled: bool,
        variant: Option<&str>,
        locally_evaluated: bool,
    ) -> EvaluatedFlagRecord {
        EvaluatedFlagRecord {
            key: key.into(),
            enabled,
            variant: variant.map(str::to_string),
            payload: None,
            id: Some(42),
            version: Some(7),
            reason: Some("condition match".into()),
            locally_evaluated,
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
            None,
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
    fn locally_evaluated_event_omits_evaluated_at_and_includes_definitions_loaded_at() {
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
            Some(1699999000),
        );
        let _ = snap.is_enabled("gamma");
        let captured = host.captured.lock().unwrap();
        let props = &captured[0].properties;
        assert_eq!(props.get("locally_evaluated"), Some(&json!(true)));
        assert_eq!(
            props.get("$feature_flag_reason"),
            Some(&json!("Evaluated locally"))
        );
        assert_eq!(
            props.get("$feature_flag_definitions_loaded_at"),
            Some(&json!(1699999000_i64))
        );
        assert!(!props.contains_key("$feature_flag_evaluated_at"));
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
    fn only_accessed_falls_back_to_all_with_warning_when_empty() {
        let host = Arc::new(RecordingHost::default());
        let snap = build(
            Arc::clone(&host) as Arc<dyn FeatureFlagEvaluationsHost>,
            "u1",
        );
        let filtered = snap.only_accessed();
        assert_eq!(filtered.keys().len(), 3);
        assert_eq!(host.warnings.lock().unwrap().len(), 1);
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

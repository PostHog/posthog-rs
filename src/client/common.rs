use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use crate::client::BeforeSendHook;
use crate::client::CaptureDefaults;
use crate::client::FlagsFailure;
use crate::client::OnErrorHook;
use crate::client::PostHogError;
use crate::feature_flag_evaluations::{
    EvaluateFlagsOptions, EvaluatedFlagRecord, FeatureFlagEvaluations, FeatureFlagEvaluationsHost,
    FlagCalledEventParams,
};
use crate::feature_flags::{FeatureFlagsResponse, FlagDetail, FlagMetadata, FlagValue};
use crate::local_evaluation::LocalEvaluator;
use crate::Error;
use crate::Event;
use tracing::{debug, error, warn};

use super::transport::TransportHandle;

/// Cap on the number of `distinct_id` entries in the `$feature_flag_called`
/// dedup cache. On overflow the entire map is reset (matches the JS SDK).
const MAX_FLAG_CALLED_CACHE_SIZE: usize = 50_000;

type FlagEventDedupCache = Mutex<HashMap<String, HashSet<String>>>;

struct RuntimeContext {
    os: String,
    os_version: String,
}

static RUNTIME_CONTEXT: OnceLock<RuntimeContext> = OnceLock::new();

fn runtime_context() -> &'static RuntimeContext {
    RUNTIME_CONTEXT.get_or_init(|| {
        let info = os_info::get();
        RuntimeContext {
            os: info.os_type().to_string(),
            os_version: info.version().to_string(),
        }
    })
}

pub(super) fn apply_runtime_context(event: &mut Event) {
    let context = runtime_context();
    event.insert_prop_default("$os", serde_json::Value::String(context.os.clone()));
    event.insert_prop_default(
        "$os_version",
        serde_json::Value::String(context.os_version.clone()),
    );
}

fn flag_event_dedup_cache() -> FlagEventDedupCache {
    Mutex::new(HashMap::new())
}

/// Runtime-neutral host for deduplicating and enqueueing
/// `$feature_flag_called` events. Each client lazily constructs its own host so
/// snapshots from that client share one dedup cache without sharing across
/// clients.
pub(super) struct FlagEventHost {
    defaults: CaptureDefaults,
    transport: Option<Arc<TransportHandle>>,
    dedup_cache: FlagEventDedupCache,
}

impl FlagEventHost {
    pub(super) fn new(defaults: CaptureDefaults, transport: Option<Arc<TransportHandle>>) -> Self {
        Self {
            defaults,
            transport,
            dedup_cache: flag_event_dedup_cache(),
        }
    }
}

impl FeatureFlagEvaluationsHost for FlagEventHost {
    fn capture_flag_called_event_if_needed(&self, params: FlagCalledEventParams) {
        let dedup_key = build_dedup_key(&params.key, params.response.as_ref(), &params.groups);
        if already_reported(&self.dedup_cache, &params.distinct_id, &dedup_key) {
            return;
        }

        if let (Some(transport), Some(event)) =
            (&self.transport, flag_called_event(params, &self.defaults))
        {
            transport.enqueue(event);
        }
    }

    fn log_warning(&self, message: &str) {
        // Surface filter-helper misuse via tracing — users can silence these
        // with their tracing-subscriber level filter (e.g. `posthog=error`).
        warn!("{message}");
    }
}

fn apply_capture_defaults(event: &mut Event, defaults: &CaptureDefaults) {
    if defaults.disable_geoip {
        event.insert_prop_default("$geoip_disable", serde_json::Value::Bool(true));
    }
    if defaults.is_server {
        event.insert_prop_default("$is_server", serde_json::Value::Bool(true));
    }
}

pub(super) fn preprocess_capture_event(
    mut event: Event,
    defaults: &CaptureDefaults,
    hooks: &[BeforeSendHook],
) -> Option<Event> {
    apply_capture_defaults(&mut event, defaults);
    apply_before_send_hooks(hooks, event)
}

fn apply_before_send_hooks(hooks: &[BeforeSendHook], event: Event) -> Option<Event> {
    let mut current = Some(event);

    for hook in hooks {
        let event = current.take().expect("event is present between hooks");
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook.apply(event))) {
            Ok(Some(next)) => current = Some(next),
            Ok(None) => return None,
            Err(_) => {
                error!("panic in PostHog before_send hook; dropping event");
                return None;
            }
        }
    }

    current
}

/// Invoke each `on_error` hook with the failure, catching panics so a
/// misbehaving hook can't wedge the caller (the transport worker, a flags
/// request, or the poller). No-op when no hooks are registered, keeping the
/// common (hookless) failure path allocation-free.
pub(crate) fn apply_on_error_hooks(hooks: &[OnErrorHook], failure: &PostHogError<'_>) {
    if hooks.is_empty() {
        return;
    }
    for hook in hooks {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook.apply(failure))).is_err() {
            error!("panic in PostHog on_error hook; ignoring");
        }
    }
}

/// Fire the `on_error` hooks for a failed `/flags` request. Each failed request
/// reports exactly once, from the leaf that finalizes the [`Error`], so a caller
/// that degrades gracefully (e.g. [`Client::evaluate_flags`](crate::Client::evaluate_flags)
/// falling back to local results) still surfaces the failure. No-op when no
/// hooks are registered.
pub(crate) fn report_flags_error(
    hooks: &[OnErrorHook],
    endpoint: &str,
    distinct_id: Option<&str>,
    status: Option<u16>,
    body: Option<&str>,
    error: &Error,
) {
    if hooks.is_empty() {
        return;
    }
    let failure = PostHogError::FeatureFlags(FlagsFailure {
        error,
        endpoint,
        distinct_id,
        status,
        body,
    });
    apply_on_error_hooks(hooks, &failure);
}

/// Returns `true` when the helper has already shipped this
/// `(distinct_id, key, response)` combination and the caller should skip.
fn already_reported(cache: &FlagEventDedupCache, distinct_id: &str, dedup_key: &str) -> bool {
    let mut cache = cache.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(seen) = cache.get(distinct_id) {
        if seen.contains(dedup_key) {
            return true;
        }
    }
    if cache.len() >= MAX_FLAG_CALLED_CACHE_SIZE {
        cache.clear();
    }
    cache
        .entry(distinct_id.to_string())
        .or_default()
        .insert(dedup_key.to_string());
    false
}

fn build_dedup_key(
    flag_key: &str,
    response: Option<&FlagValue>,
    groups: &HashMap<String, String>,
) -> String {
    let response_repr = match response {
        Some(FlagValue::Boolean(true)) => "true".to_string(),
        Some(FlagValue::Boolean(false)) => "false".to_string(),
        Some(FlagValue::String(s)) => s.clone(),
        None => "::null::".to_string(),
    };
    if groups.is_empty() {
        format!("{flag_key}_{response_repr}")
    } else {
        // Canonicalize so two equal group maps with different insertion orders
        // produce the same dedup key — necessary for group-scoped flags to fire
        // exactly once per distinct group context.
        let mut sorted: Vec<(&String, &String)> = groups.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        let groups_repr: String = sorted
            .iter()
            .map(|(k, v)| format!("{}={}", pct(k), pct(v)))
            .collect::<Vec<_>>()
            .join(";");
        format!("{flag_key}_{response_repr}_{groups_repr}")
    }
}

fn pct(s: &str) -> String {
    s.replace('%', "%25")
        .replace('=', "%3D")
        .replace(';', "%3B")
}

fn flag_called_event(params: FlagCalledEventParams, defaults: &CaptureDefaults) -> Option<Event> {
    let mut event = Event::new("$feature_flag_called".to_string(), params.distinct_id);
    if params.minimal {
        // Marks the event so the capture pipeline trims it to the minimal
        // allowlist as its final enrichment step, after system context and SDK
        // metadata are stamped on.
        event.mark_minimal_flag_called();
    }
    for (k, v) in params.properties {
        if event.insert_prop(k, v).is_err() {
            return None;
        }
    }
    for (group_name, group_id) in &params.groups {
        event.add_group(group_name, group_id);
    }
    if params.disable_geoip.unwrap_or(defaults.disable_geoip) {
        event.insert_prop_default("$geoip_disable", serde_json::Value::Bool(true));
    }
    if defaults.is_server {
        event.insert_prop_default("$is_server", serde_json::Value::Bool(true));
    }
    Some(event)
}

/// Normalised view of a `/flags?v=2` response surfacing the per-flag detail
/// shape needed by the snapshot path.
pub(super) struct DetailedFlagsResponse {
    pub(super) flags: HashMap<String, FlagDetail>,
    pub(super) request_id: Option<String>,
    pub(super) errors_while_computing_flags: bool,
    pub(super) quota_limited: bool,
    /// The `minimalFlagCalledEvents` gate as reported by this `/flags?v=2`
    /// response. Pinned here so it travels with the flags it evaluated, rather
    /// than being re-read from shared state when an event is later captured.
    pub(super) minimal_flag_called_events: bool,
}

pub(super) fn extract_flag_details(response: FeatureFlagsResponse) -> DetailedFlagsResponse {
    match response {
        FeatureFlagsResponse::V2 {
            flags,
            request_id,
            errors_while_computing_flags,
            quota_limited,
            minimal_flag_called_events,
        } => DetailedFlagsResponse {
            flags,
            request_id,
            errors_while_computing_flags,
            quota_limited,
            minimal_flag_called_events,
        },
        FeatureFlagsResponse::Legacy {
            feature_flags,
            feature_flag_payloads,
            errors,
        } => {
            let mut flags = HashMap::new();
            for (key, value) in feature_flags {
                let (enabled, variant) = match value {
                    FlagValue::Boolean(b) => (b, None),
                    FlagValue::String(s) => (true, Some(s)),
                };
                let payload = feature_flag_payloads.get(&key).cloned();
                flags.insert(
                    key.clone(),
                    FlagDetail {
                        key,
                        enabled,
                        variant,
                        reason: None,
                        metadata: payload.map(|payload| FlagMetadata {
                            id: 0,
                            version: 0,
                            description: None,
                            payload: Some(payload),
                            has_experiment: None,
                        }),
                    },
                );
            }
            DetailedFlagsResponse {
                flags,
                request_id: None,
                errors_while_computing_flags: errors.is_some_and(|e| !e.is_empty()),
                quota_limited: false,
                // The legacy response shape carries no minimization gate, so it
                // fails safe to the full event shape.
                minimal_flag_called_events: false,
            }
        }
    }
}

/// Shared policy for combining local and remote flag evaluations. The clients
/// retain their concrete HTTP and async/blocking boundaries; this state owns
/// only the runtime-neutral decisions on whether to fetch and how to merge.
pub(super) struct EvaluationState {
    distinct_id: String,
    options: EvaluateFlagsOptions,
    records: HashMap<String, EvaluatedFlagRecord>,
    request_id: Option<String>,
    errors_while_computing: bool,
    quota_limited: bool,
}

impl EvaluationState {
    pub(super) fn new(
        distinct_id: String,
        mut options: EvaluateFlagsOptions,
        local_evaluator: Option<&LocalEvaluator>,
    ) -> Self {
        options.groups.get_or_insert_with(HashMap::new);
        options.group_properties.get_or_insert_with(HashMap::new);

        let mut state = Self {
            distinct_id,
            options,
            records: HashMap::new(),
            request_id: None,
            errors_while_computing: false,
            quota_limited: false,
        };
        state.evaluate_locally(local_evaluator);
        state
    }

    fn evaluate_locally(&mut self, local_evaluator: Option<&LocalEvaluator>) {
        let Some(evaluator) = local_evaluator else {
            return;
        };

        let mut person_properties = self.options.person_properties.clone().unwrap_or_default();
        person_properties
            .entry("distinct_id".to_string())
            .or_insert_with(|| serde_json::json!(self.distinct_id.clone()));
        let groups = self.options.groups.clone().unwrap_or_default();
        let group_properties = self.options.group_properties.clone().unwrap_or_default();
        let local_results = evaluator.evaluate_all_flags_with_details(
            &self.distinct_id,
            &person_properties,
            &groups,
            &group_properties,
        );

        // Pin the gate from the definitions snapshot that produced each local
        // result rather than re-reading shared state when an event is captured.
        let minimal_flag_called_events = evaluator.cache().minimal_flag_called_events();
        for (key, result) in local_results {
            if self
                .options
                .flag_keys
                .as_ref()
                .is_some_and(|filter| !filter.iter().any(|candidate| candidate == &key))
            {
                continue;
            }
            if let Ok(value) = result.result {
                self.records.insert(
                    key,
                    local_record(
                        value,
                        result.payload,
                        result.has_experiment,
                        minimal_flag_called_events,
                    ),
                );
            }
        }
    }

    /// Without an explicit key filter, local definitions cannot prove that they
    /// contain every project flag, so remote discovery remains necessary.
    pub(super) fn should_fetch_remote(&self, local_evaluation_only: bool) -> bool {
        let local_covers_request = self
            .options
            .flag_keys
            .as_ref()
            .is_some_and(|keys| keys.iter().all(|key| self.records.contains_key(key)));

        !self.options.only_evaluate_locally && !local_evaluation_only && !local_covers_request
    }

    pub(super) fn distinct_id(&self) -> &str {
        &self.distinct_id
    }

    pub(super) fn options(&self) -> &EvaluateFlagsOptions {
        &self.options
    }

    pub(super) fn apply_remote_result(
        &mut self,
        result: Result<DetailedFlagsResponse, Error>,
    ) -> Result<(), Error> {
        match result {
            Ok(response) => {
                self.request_id = response.request_id;
                self.errors_while_computing = response.errors_while_computing_flags;
                self.quota_limited = response.quota_limited;
                let minimal_flag_called_events = response.minimal_flag_called_events;
                for (key, detail) in response.flags {
                    // A successful local evaluation is authoritative. Remote
                    // evaluation only fills flags that local evaluation could
                    // not resolve.
                    self.records.entry(key).or_insert_with(|| {
                        remote_record_from_detail(detail, minimal_flag_called_events)
                    });
                }
                Ok(())
            }
            Err(error) if self.records.is_empty() => Err(error),
            Err(error) => {
                debug!(
                    error = error.to_string(),
                    local_count = self.records.len(),
                    "/flags fetch failed; returning snapshot from local results only"
                );
                self.errors_while_computing = true;
                Ok(())
            }
        }
    }

    pub(super) fn into_evaluations(
        self,
        host: Arc<dyn FeatureFlagEvaluationsHost>,
    ) -> FeatureFlagEvaluations {
        FeatureFlagEvaluations::new(
            host,
            self.distinct_id,
            self.records,
            self.options.groups.unwrap_or_default(),
            self.options.disable_geoip,
            self.request_id,
            None,
            self.errors_while_computing,
            self.quota_limited,
        )
    }
}

fn local_record(
    value: FlagValue,
    payload: Option<serde_json::Value>,
    has_experiment: Option<bool>,
    minimal_flag_called_events: bool,
) -> EvaluatedFlagRecord {
    let (enabled, variant) = match value {
        FlagValue::Boolean(b) => (b, None),
        FlagValue::String(s) => (true, Some(s)),
    };
    EvaluatedFlagRecord {
        enabled,
        variant,
        // The definitions manifest stores payloads the same way `/flags`
        // returns them, so they go through the same normalisation.
        payload: payload.map(normalize_payload),
        id: None,
        version: None,
        reason: Some("Evaluated locally".to_string()),
        locally_evaluated: true,
        has_experiment,
        minimal_flag_called_events,
    }
}

fn remote_record_from_detail(
    detail: FlagDetail,
    minimal_flag_called_events: bool,
) -> EvaluatedFlagRecord {
    let metadata = detail.metadata;
    let reason = detail
        .reason
        .and_then(|r| r.description.or(Some(r.code)))
        .filter(|s| !s.is_empty());
    let id = metadata.as_ref().map(|m| m.id);
    let version = metadata.as_ref().map(|m| m.version);
    let has_experiment = metadata.as_ref().and_then(|m| m.has_experiment);
    let payload = metadata.and_then(|m| m.payload).map(normalize_payload);
    EvaluatedFlagRecord {
        enabled: detail.enabled,
        variant: detail.variant,
        payload,
        id,
        version,
        reason,
        locally_evaluated: false,
        has_experiment,
        minimal_flag_called_events,
    }
}

/// `metadata.payload` from `/flags?v=2` is sometimes a JSON-encoded string
/// (e.g. `"{\"color\":\"blue\"}"`) rather than already-parsed JSON. Try to
/// parse a `String` payload as JSON and fall back to the raw string on
/// failure so users can branch on a uniform [`serde_json::Value`].
fn normalize_payload(payload: serde_json::Value) -> serde_json::Value {
    match payload {
        serde_json::Value::String(raw) => {
            serde_json::from_str(&raw).unwrap_or(serde_json::Value::String(raw))
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn groups(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn defaults(disable_geoip: bool, is_server: bool) -> CaptureDefaults {
        CaptureDefaults {
            disable_geoip,
            is_server,
        }
    }

    fn local_evaluator() -> LocalEvaluator {
        use crate::feature_flags::{FeatureFlag, FeatureFlagCondition, FeatureFlagFilters};
        use crate::local_evaluation::{FlagCache, LocalEvaluationResponse};

        let flag = |key: &str| FeatureFlag {
            key: key.to_string(),
            active: true,
            has_experiment: Some(false),
            filters: FeatureFlagFilters {
                groups: vec![FeatureFlagCondition {
                    properties: Vec::new(),
                    rollout_percentage: Some(100.0),
                    variant: None,
                    aggregation_group_type_index: None,
                }],
                multivariate: None,
                payloads: HashMap::new(),
                aggregation_group_type_index: None,
                early_exit: false,
            },
        };

        let cache = FlagCache::new();
        cache.update(LocalEvaluationResponse {
            flags: vec![flag("local"), flag("filtered-out")],
            group_type_mapping: HashMap::new(),
            cohorts: HashMap::new(),
            minimal_flag_called_events: true,
        });
        LocalEvaluator::new(cache)
    }

    fn remote_detail(key: &str, enabled: bool, id: u64) -> FlagDetail {
        FlagDetail {
            key: key.to_string(),
            enabled,
            variant: None,
            reason: Some(crate::feature_flags::FlagReason {
                code: "condition_match".to_string(),
                condition_index: Some(0),
                description: Some("Matched remotely".to_string()),
            }),
            metadata: Some(FlagMetadata {
                id,
                version: 7,
                description: None,
                payload: Some(json!({"source": "remote"})),
                has_experiment: Some(true),
            }),
        }
    }

    #[test]
    fn evaluation_state_filters_local_results_and_preserves_local_wins_merge_metadata() {
        let evaluator = local_evaluator();
        let mut options = EvaluateFlagsOptions::default();
        options.flag_keys = Some(vec!["local".to_string(), "remote".to_string()]);
        options.groups = Some(groups(&[("organization", "org-a")]));

        let mut state = EvaluationState::new("user-1".to_string(), options, Some(&evaluator));
        assert_eq!(state.records.len(), 1);
        assert!(state.records.contains_key("local"));
        assert!(!state.records.contains_key("filtered-out"));
        assert!(state.should_fetch_remote(false));

        let local_before_merge = &state.records["local"];
        assert!(local_before_merge.enabled);
        assert!(local_before_merge.locally_evaluated);
        assert!(local_before_merge.minimal_flag_called_events);

        let response = DetailedFlagsResponse {
            flags: HashMap::from([
                ("local".to_string(), remote_detail("local", false, 101)),
                ("remote".to_string(), remote_detail("remote", true, 202)),
            ]),
            request_id: Some("req-policy".to_string()),
            errors_while_computing_flags: true,
            quota_limited: true,
            minimal_flag_called_events: false,
        };
        state.apply_remote_result(Ok(response)).unwrap();

        let local = &state.records["local"];
        assert!(local.enabled, "successful local evaluation must win");
        assert!(local.locally_evaluated);
        assert_eq!(local.id, None);
        assert!(local.minimal_flag_called_events);

        let remote = &state.records["remote"];
        assert!(remote.enabled);
        assert!(!remote.locally_evaluated);
        assert_eq!(remote.id, Some(202));
        assert_eq!(remote.version, Some(7));
        assert_eq!(remote.reason.as_deref(), Some("Matched remotely"));
        assert_eq!(remote.payload, Some(json!({"source": "remote"})));
        assert_eq!(remote.has_experiment, Some(true));
        assert!(!remote.minimal_flag_called_events);

        assert_eq!(state.request_id.as_deref(), Some("req-policy"));
        assert!(state.errors_while_computing);
        assert!(state.quota_limited);
        assert_eq!(
            state.options.groups.as_ref().unwrap().get("organization"),
            Some(&"org-a".to_string())
        );
    }

    #[test]
    fn evaluation_state_characterizes_fetch_gates_and_partial_fallback() {
        let evaluator = local_evaluator();

        let mut covered_options = EvaluateFlagsOptions::default();
        covered_options.flag_keys = Some(vec!["local".to_string()]);
        let covered = EvaluationState::new("user-1".to_string(), covered_options, Some(&evaluator));
        assert!(!covered.should_fetch_remote(false));

        let mut local_only_options = EvaluateFlagsOptions::default();
        local_only_options.only_evaluate_locally = true;
        let local_only =
            EvaluationState::new("user-1".to_string(), local_only_options, Some(&evaluator));
        assert!(!local_only.should_fetch_remote(false));

        let project_local_only = EvaluationState::new(
            "user-1".to_string(),
            EvaluateFlagsOptions::default(),
            Some(&evaluator),
        );
        assert!(!project_local_only.should_fetch_remote(true));

        let mut partial_options = EvaluateFlagsOptions::default();
        partial_options.flag_keys = Some(vec!["local".to_string(), "missing".to_string()]);
        let mut partial =
            EvaluationState::new("user-1".to_string(), partial_options, Some(&evaluator));
        partial
            .apply_remote_result(Err(Error::Connection("remote failed".to_string())))
            .expect("local results degrade a remote failure to a partial snapshot");
        assert_eq!(partial.records.len(), 1);
        assert!(partial.records.contains_key("local"));
        assert!(partial.errors_while_computing);
        assert_eq!(partial.request_id, None);
        assert!(!partial.quota_limited);

        let mut empty =
            EvaluationState::new("user-1".to_string(), EvaluateFlagsOptions::default(), None);
        assert!(empty
            .apply_remote_result(Err(Error::Connection("remote failed".to_string())))
            .is_err());
    }

    fn flag_params(
        properties: HashMap<String, serde_json::Value>,
        groups: HashMap<String, String>,
        disable_geoip: Option<bool>,
    ) -> FlagCalledEventParams {
        FlagCalledEventParams {
            distinct_id: "user-1".to_string(),
            key: "alpha".to_string(),
            response: Some(FlagValue::Boolean(true)),
            groups,
            disable_geoip,
            properties,
            minimal: false,
        }
    }

    #[test]
    fn dedup_key_canonicalizes_group_order_and_escapes_separators() {
        let first = build_dedup_key(
            "alpha",
            Some(&FlagValue::Boolean(true)),
            &groups(&[("organization", "org-a"), ("team", "red")]),
        );
        let second = build_dedup_key(
            "alpha",
            Some(&FlagValue::Boolean(true)),
            &groups(&[("team", "red"), ("organization", "org-a")]),
        );
        assert_eq!(first, second);

        let with_separator_in_key = build_dedup_key(
            "alpha",
            Some(&FlagValue::Boolean(true)),
            &groups(&[("a=b", "c")]),
        );
        let with_separator_in_value = build_dedup_key(
            "alpha",
            Some(&FlagValue::Boolean(true)),
            &groups(&[("a", "b=c")]),
        );
        assert_ne!(with_separator_in_key, with_separator_in_value);
    }

    #[test]
    fn flag_called_event_applies_defaults_groups_and_preserves_caller_properties() {
        let mut properties = HashMap::new();
        properties.insert("$is_server".to_string(), json!(false));
        properties.insert("$geoip_disable".to_string(), json!(false));

        let event = flag_called_event(
            flag_params(properties, groups(&[("organization", "org-a")]), Some(true)),
            &defaults(true, true),
        )
        .expect("valid flag-called event");

        assert_eq!(
            event.groups().get("organization"),
            Some(&"org-a".to_string())
        );
        assert_eq!(event.properties().get("$is_server"), Some(&json!(false)));
        assert_eq!(
            event.properties().get("$geoip_disable"),
            Some(&json!(false))
        );
    }

    #[test]
    fn flag_called_event_adds_defaults_when_missing() {
        let event = flag_called_event(
            flag_params(HashMap::new(), HashMap::new(), None),
            &defaults(true, true),
        )
        .expect("valid flag-called event");

        assert_eq!(event.properties().get("$is_server"), Some(&json!(true)));
        assert_eq!(event.properties().get("$geoip_disable"), Some(&json!(true)));
    }

    #[test]
    fn flag_event_hosts_keep_dedup_state_separate() {
        let first = FlagEventHost::new(defaults(false, true), None);
        let second = FlagEventHost::new(defaults(false, true), None);
        let params = flag_params(HashMap::new(), HashMap::new(), None);

        first.capture_flag_called_event_if_needed(params.clone());
        first.capture_flag_called_event_if_needed(params.clone());

        let first_cache = first.dedup_cache.lock().unwrap();
        assert_eq!(first_cache.get("user-1").map(HashSet::len), Some(1));
        drop(first_cache);
        assert!(second.dedup_cache.lock().unwrap().is_empty());

        second.capture_flag_called_event_if_needed(params);
        assert_eq!(
            second
                .dedup_cache
                .lock()
                .unwrap()
                .get("user-1")
                .map(HashSet::len),
            Some(1)
        );
    }

    #[test]
    fn runtime_context_adds_missing_os_properties_only() {
        let mut event = Event::new("test", "user-1");
        event.insert_prop("$os", "custom-os").unwrap();

        apply_runtime_context(&mut event);

        assert_eq!(event.properties().get("$os"), Some(&json!("custom-os")));
        assert!(event.properties().contains_key("$os_version"));
        assert!(!event.properties().contains_key("$os_arch"));
    }

    #[test]
    fn flag_called_event_leaves_runtime_context_to_capture_path() {
        let event = flag_called_event(
            flag_params(HashMap::new(), HashMap::new(), None),
            &defaults(false, true),
        )
        .expect("valid flag-called event");

        assert!(!event.properties().contains_key("$os"));
        assert!(!event.properties().contains_key("$os_version"));
        assert!(!event.properties().contains_key("$os_arch"));
    }

    #[test]
    fn before_send_hooks_mutate_and_drop_events() {
        let options = crate::ClientOptionsBuilder::default()
            .api_key("test-key".to_string())
            .before_send(|mut event| {
                event.insert_prop("from_hook", true).unwrap();
                Some(event)
            })
            .before_send(|event| {
                if event.event_name() == "drop" {
                    None
                } else {
                    Some(event)
                }
            })
            .build()
            .unwrap();

        let event = apply_before_send_hooks(&options.before_send, Event::new("keep", "user-1"))
            .expect("event should be kept");
        assert_eq!(event.properties().get("from_hook"), Some(&json!(true)));

        assert!(
            apply_before_send_hooks(&options.before_send, Event::new("drop", "user-1")).is_none()
        );
    }

    #[test]
    fn before_send_hook_panic_drops_event() {
        let options = crate::ClientOptionsBuilder::default()
            .api_key("test-key".to_string())
            .before_send(|_event| panic!("boom"))
            .build()
            .unwrap();

        assert!(
            apply_before_send_hooks(&options.before_send, Event::new("test", "user-1")).is_none()
        );
    }
}

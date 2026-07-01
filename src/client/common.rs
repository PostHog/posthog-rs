use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::client::BeforeSendHook;
use crate::client::CaptureDefaults;
use crate::client::FlagsFailure;
use crate::client::OnErrorHook;
use crate::client::PostHogError;
use crate::feature_flag_evaluations::{EvaluatedFlagRecord, FlagCalledEventParams};
use crate::feature_flags::{FeatureFlagsResponse, FlagDetail, FlagMetadata, FlagValue};
use crate::Error;
use crate::Event;
use tracing::error;

/// Cap on the number of `distinct_id` entries in the `$feature_flag_called`
/// dedup cache. On overflow the entire map is reset (matches the JS SDK).
pub(super) const MAX_FLAG_CALLED_CACHE_SIZE: usize = 50_000;

pub(super) type FlagEventDedupCache = Mutex<HashMap<String, HashSet<String>>>;

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

pub(super) fn flag_event_dedup_cache() -> FlagEventDedupCache {
    Mutex::new(HashMap::new())
}

pub(super) fn apply_capture_defaults(event: &mut Event, defaults: &CaptureDefaults) {
    if defaults.disable_geoip {
        event.insert_prop_default("$geoip_disable", serde_json::Value::Bool(true));
    }
    if defaults.is_server {
        event.insert_prop_default("$is_server", serde_json::Value::Bool(true));
    }
}

pub(super) fn apply_before_send_hooks(hooks: &[BeforeSendHook], event: Event) -> Option<Event> {
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
pub(super) fn already_reported(
    cache: &FlagEventDedupCache,
    distinct_id: &str,
    dedup_key: &str,
) -> bool {
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

pub(super) fn build_dedup_key(
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

pub(super) fn flag_called_event(
    params: FlagCalledEventParams,
    client_disable_geoip: bool,
    is_server: bool,
) -> Option<Event> {
    let mut event = Event::new("$feature_flag_called".to_string(), params.distinct_id);
    for (k, v) in params.properties {
        if event.insert_prop(k, v).is_err() {
            return None;
        }
    }
    for (group_name, group_id) in &params.groups {
        event.add_group(group_name, group_id);
    }
    if params.disable_geoip.unwrap_or(client_disable_geoip) {
        event.insert_prop_default("$geoip_disable", serde_json::Value::Bool(true));
    }
    if is_server {
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
}

pub(super) fn extract_flag_details(response: FeatureFlagsResponse) -> DetailedFlagsResponse {
    match response {
        FeatureFlagsResponse::V2 {
            flags,
            request_id,
            errors_while_computing_flags,
            quota_limited,
        } => DetailedFlagsResponse {
            flags,
            request_id,
            errors_while_computing_flags,
            quota_limited,
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
                        }),
                    },
                );
            }
            DetailedFlagsResponse {
                flags,
                request_id: None,
                errors_while_computing_flags: errors.is_some_and(|e| !e.is_empty()),
                quota_limited: false,
            }
        }
    }
}

pub(super) fn local_record(value: FlagValue) -> EvaluatedFlagRecord {
    let (enabled, variant) = match value {
        FlagValue::Boolean(b) => (b, None),
        FlagValue::String(s) => (true, Some(s)),
    };
    EvaluatedFlagRecord {
        enabled,
        variant,
        // Local definitions do not surface a payload through the poller today.
        payload: None,
        id: None,
        version: None,
        reason: Some("Evaluated locally".to_string()),
        locally_evaluated: true,
    }
}

pub(super) fn remote_record_from_detail(detail: FlagDetail) -> EvaluatedFlagRecord {
    let metadata = detail.metadata;
    let reason = detail
        .reason
        .and_then(|r| r.description.or(Some(r.code)))
        .filter(|s| !s.is_empty());
    let id = metadata.as_ref().map(|m| m.id);
    let version = metadata.as_ref().map(|m| m.version);
    let payload = metadata.and_then(|m| m.payload).map(normalize_payload);
    EvaluatedFlagRecord {
        enabled: detail.enabled,
        variant: detail.variant,
        payload,
        id,
        version,
        reason,
        locally_evaluated: false,
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
            true,
            true,
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
            true,
            true,
        )
        .expect("valid flag-called event");

        assert_eq!(event.properties().get("$is_server"), Some(&json!(true)));
        assert_eq!(event.properties().get("$geoip_disable"), Some(&json!(true)));
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
            false,
            true,
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

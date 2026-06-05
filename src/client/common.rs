use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::feature_flag_evaluations::{EvaluatedFlagRecord, FlagCalledEventParams};
use crate::feature_flags::{FeatureFlagsResponse, FlagDetail, FlagMetadata, FlagValue};
use crate::Event;

/// Cap on the number of `distinct_id` entries in the `$feature_flag_called`
/// dedup cache. On overflow the entire map is reset (matches the JS SDK).
pub(super) const MAX_FLAG_CALLED_CACHE_SIZE: usize = 50_000;

pub(super) type FlagEventDedupCache = Mutex<HashMap<String, HashSet<String>>>;

pub(super) fn flag_event_dedup_cache() -> FlagEventDedupCache {
    Mutex::new(HashMap::new())
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

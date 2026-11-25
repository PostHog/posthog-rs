//! Manual recording permits only remotely enabled, unrestricted configurations.
//! Like posthog-ios and posthog-android, it ignores the browser-only fields:
//! URL triggers and blocklists, trigger groups and the config version.
use serde_json::Value;

/// The upload path from remote config, which the other SDKs post to, or
/// `/s/` when it names none.
pub(super) fn endpoint(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_object)
        .and_then(|fields| fields.get("endpoint"))
        .and_then(Value::as_str)
        .filter(|path| path.starts_with('/'))
        .unwrap_or("/s/")
        .to_owned()
}

pub(super) fn eligible(value: Option<&Value>) -> bool {
    let Some(value) = value else { return false };
    if let Some(enabled) = value.as_bool() {
        return enabled;
    }
    let Some(fields) = value.as_object() else {
        return false;
    };
    let optional = |name: &str| fields.get(name).filter(|v| !v.is_null());
    if optional("enabled").is_some_and(|v| v.as_bool() != Some(true)) {
        return false;
    }
    if optional("linkedFlag").is_some() {
        return false;
    }
    if optional("sampleRate")
        .is_some_and(|v| v.as_str().and_then(|s| s.parse::<f64>().ok()) != Some(1.0))
    {
        return false;
    }
    if optional("minimumDurationMilliseconds").is_some_and(|v| v.as_f64() != Some(0.0)) {
        return false;
    }
    if optional("eventTriggers").is_some_and(|v| !v.as_array().is_some_and(Vec::is_empty)) {
        return false;
    }
    if optional("triggerMatchType").is_some_and(|v| !matches!(v.as_str(), Some("all" | "any"))) {
        return false;
    }
    true
}

/// `$sdk_debug_replay_*_trigger_status` for one trigger kind. This client never
/// evaluates triggers, so a configured one stays pending and recording stays
/// off; an unconfigured one is disabled.
pub(super) fn trigger_status(value: Option<&Value>, name: &str) -> &'static str {
    let configured = value
        .and_then(Value::as_object)
        .and_then(|fields| fields.get(name))
        .is_some_and(|v| match v {
            Value::Null => false,
            Value::Array(items) => !items.is_empty(),
            _ => true,
        });
    if configured {
        "trigger_pending"
    } else {
        "trigger_disabled"
    }
}

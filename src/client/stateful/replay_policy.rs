//! Manual recording permits only remotely enabled, unrestricted configurations.
use serde_json::Value;

pub(super) fn eligible(value: Option<&Value>) -> bool {
    let Some(value) = value else { return false };
    if let Some(enabled) = value.as_bool() {
        return enabled;
    }
    let Some(fields) = value.as_object() else {
        return false;
    };
    let optional = |name: &str| fields.get(name).filter(|v| !v.is_null());
    if optional("version").is_some_and(|v| v.as_u64() != Some(1)) {
        return false;
    }
    if optional("triggerGroups").is_some_and(|v| !v.as_array().is_some_and(Vec::is_empty)) {
        return false;
    }
    if optional("enabled").is_some_and(|v| v.as_bool() != Some(true)) {
        return false;
    }
    if optional("endpoint").is_some_and(|v| v.as_str() != Some("/s/")) {
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
    for name in ["eventTriggers", "urlTriggers", "urlBlocklist"] {
        if optional(name).is_some_and(|v| !v.as_array().is_some_and(Vec::is_empty)) {
            return false;
        }
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

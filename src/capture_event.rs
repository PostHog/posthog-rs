use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;
use uuid::Uuid;

use crate::constants::{OptionKind, OPTIONS_EXTRACTION_TABLE, SESSION_ID_PROP, WINDOW_ID_PROP};
use crate::event::Event;

/// Crate-internal capture options, derived from `event.properties`.
/// Serializes as a JSON object; an empty map produces `"options":{}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Options(serde_json::Map<String, serde_json::Value>);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureEvent {
    pub event: String,
    pub uuid: Uuid,
    pub distinct_id: String,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    pub(crate) options: Options,
    pub properties: serde_json::Value,
}

impl CaptureEvent {
    // Only the tests build a CaptureEvent without an injected clock; the worker uses
    // `from_event_at` so its timestamps are deterministic.
    #[cfg(test)]
    pub fn from_event(event: &Event) -> Self {
        Self::from_event_at(event, Utc::now())
    }

    /// Like [`CaptureEvent::from_event`] but with an injected `now`, so the transport
    /// worker can stamp a deterministic timestamp from its clock when the event
    /// carries none of its own.
    pub(crate) fn from_event_at(event: &Event, now: DateTime<Utc>) -> Self {
        let mut properties = event.properties().clone();

        if !event.groups().is_empty() {
            properties.insert(
                "$groups".into(),
                serde_json::Value::Object(
                    event
                        .groups()
                        .iter()
                        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                        .collect(),
                ),
            );
        }

        let timestamp = event
            .timestamp()
            .map(|ts| ts.and_utc().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
            .unwrap_or_else(|| now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string());

        // Extract magic keys from properties into the wire options map. The key is
        // always removed from properties (these sentinels must never reach the
        // backend properties); the value is coerced to the type the backend's
        // strict `Options` struct expects. A value that can't be coerced is
        // dropped so the backend applies its default, rather than 400-ing the
        // whole batch on a type mismatch.
        let mut options_map = serde_json::Map::new();
        for &(prop_key, wire_key, kind) in OPTIONS_EXTRACTION_TABLE {
            if let Some(val) = properties.remove(prop_key) {
                match coerce_option(kind, val) {
                    Some(coerced) => {
                        options_map.insert(wire_key.to_string(), coerced);
                    }
                    None => debug!(
                        prop = prop_key,
                        "capture options: dropping mistyped value; backend will apply its default"
                    ),
                }
            }
        }

        let session_id = properties
            .remove(SESSION_ID_PROP)
            .and_then(|v| v.as_str().map(String::from));
        let window_id = properties
            .remove(WINDOW_ID_PROP)
            .and_then(|v| v.as_str().map(String::from));

        Self {
            event: event.event_name().to_string(),
            uuid: event.uuid(),
            distinct_id: event.distinct_id().to_string(),
            timestamp,
            session_id,
            window_id,
            options: Options(options_map),
            properties: serde_json::to_value(properties)
                .unwrap_or(serde_json::Value::Object(Default::default())),
        }
    }
}

/// Coerce a lifted property value to the type the backend `Options` field
/// expects, returning the canonical wire value or `None` if uninterpretable.
fn coerce_option(kind: OptionKind, val: Value) -> Option<Value> {
    match kind {
        OptionKind::Bool => coerce_bool(val).map(Value::Bool),
        OptionKind::Str => coerce_string(val).map(Value::String),
    }
}

/// Coerce a value to bool, mirroring posthog-go: native bool passes through;
/// the strings `"true"`/`"1"` (and `"false"`/`"0"`), trimmed and
/// case-insensitive, are accepted; any non-zero number is true and zero is
/// false. Returns `None` when the value is not interpretable as a boolean.
fn coerce_bool(val: Value) -> Option<bool> {
    match val {
        Value::Bool(b) => Some(b),
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        Value::Number(n) => n.as_f64().map(|f| f != 0.0),
        _ => None,
    }
}

/// Coerce a value to string. The backend's `product_tour_id` is
/// `Option<String>`; non-string types are not interpretable.
fn coerce_string(val: Value) -> Option<String> {
    match val {
        Value::String(s) => Some(s),
        _ => None,
    }
}

/// Owned variant used by tests; the capture pipeline uses [`BatchRequestRef`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct BatchRequest {
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub historical_migration: Option<bool>,
    pub batch: Vec<CaptureEvent>,
}

/// Serialize-only borrowed twin of [`BatchRequest`]; avoids per-attempt clones.
#[derive(Debug, Serialize)]
pub(crate) struct BatchRequestRef<'a> {
    pub created_at: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub historical_migration: Option<bool>,
    pub batch: &'a [CaptureEvent],
}

/// Only `Retry` is resent; all other variants are terminal.
/// `Unknown` (`#[serde(other)]`) is a forward-compat catch-all that deserializes
/// unrecognised statuses as terminal rather than failing the parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum EventStatus {
    Ok,
    Drop,
    Warning,
    Retry,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventResult {
    pub result: EventStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureResponse {
    pub results: HashMap<Uuid, EventResult>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
#[non_exhaustive]
pub struct CaptureErrorResponse {
    pub error: String,
    #[serde(default)]
    pub error_description: Option<String>,
    #[serde(default)]
    pub error_uri: Option<String>,
}

/// Wire-shape tests for the capture event builder.
///
/// Note on `$lib`/`$lib_version`: they are never carried in `properties`. The
/// SDK sends its identity in the `posthog-sdk-info` header and capture
/// materializes the properties server-side, so that contract is covered by
/// `client::capture::tests::build_headers_sdk_info_is_canonical_lib_slash_version`.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Event;
    use uuid::Uuid;

    // -- from_event basics ---------------------------------------------------

    #[test]
    fn event_from_event_basic() {
        let event = Event::new("test_event", "user-1");
        let wire = CaptureEvent::from_event(&event);

        assert_eq!(wire.event, "test_event");
        assert_eq!(wire.distinct_id, "user-1");
        assert!(wire.session_id.is_none());
        assert!(wire.window_id.is_none());
        // No magic keys -> empty options map on the wire.
        let json = serde_json::to_value(&wire).unwrap();
        let options = json.get("options").unwrap().as_object().unwrap();
        assert!(options.is_empty());
    }

    #[test]
    fn event_preserves_utc_timestamp_serialization() {
        let mut event = Event::new("test_event", "user-1");
        event
            .set_timestamp(DateTime::parse_from_rfc3339("2023-01-01T10:00:00.123+03:00").unwrap())
            .unwrap();

        let wire = CaptureEvent::from_event(&event);
        assert_eq!(wire.timestamp, "2023-01-01T07:00:00.123Z");
    }

    #[test]
    fn event_from_event_anon() {
        let event = Event::new_anon("anon_event");
        let wire = CaptureEvent::from_event(&event);

        assert_eq!(wire.event, "anon_event");
        let json = serde_json::to_value(&wire).unwrap();
        let options = json.get("options").unwrap().as_object().unwrap();
        assert_eq!(
            options.get("process_person_profile"),
            Some(&serde_json::json!(false))
        );
        let props = wire.properties.as_object().unwrap();
        assert!(!props.contains_key("$process_person_profile"));
    }

    // -- property -> options extraction --------------------------------------

    #[test]
    fn event_extracts_magic_keys_to_options() {
        let mut event = Event::new("test_event", "user-1");
        event.insert_prop("$cookieless_mode", true).unwrap();
        event.insert_prop("$process_person_profile", false).unwrap();
        event.insert_prop("$product_tour_id", "tour-42").unwrap();

        let wire = CaptureEvent::from_event(&event);
        let json = serde_json::to_value(&wire).unwrap();
        let options = json.get("options").unwrap().as_object().unwrap();

        assert_eq!(
            options.get("cookieless_mode"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            options.get("process_person_profile"),
            Some(&serde_json::json!(false))
        );
        assert_eq!(
            options.get("product_tour_id"),
            Some(&serde_json::json!("tour-42"))
        );
        // disable_skew_correction not set -> absent.
        assert!(!options.contains_key("disable_skew_correction"));
        // Extracted keys removed from properties.
        let props = wire.properties.as_object().unwrap();
        assert!(!props.contains_key("$cookieless_mode"));
        assert!(!props.contains_key("$process_person_profile"));
        assert!(!props.contains_key("$product_tour_id"));
    }

    #[test]
    fn event_extracts_ignore_sent_at_as_disable_skew_correction() {
        let mut event = Event::new("test", "user-1");
        event.insert_prop("$ignore_sent_at", true).unwrap();

        let wire = CaptureEvent::from_event(&event);
        let json = serde_json::to_value(&wire).unwrap();
        let options = json.get("options").unwrap().as_object().unwrap();
        assert_eq!(
            options.get("disable_skew_correction"),
            Some(&serde_json::json!(true))
        );
        let props = wire.properties.as_object().unwrap();
        assert!(!props.contains_key("$ignore_sent_at"));
    }

    // -- options type coercion -----------------------------------------------

    /// Lift one magic property and return the parsed (options, properties)
    /// objects so a case can assert both the wire value and the strip.
    fn lift_one(
        prop_key: &str,
        val: serde_json::Value,
    ) -> (
        serde_json::Map<String, serde_json::Value>,
        serde_json::Map<String, serde_json::Value>,
    ) {
        let mut event = Event::new("test", "user-1");
        event.insert_prop(prop_key, val).unwrap();
        let wire = CaptureEvent::from_event(&event);
        let json = serde_json::to_value(&wire).unwrap();
        let options = json.get("options").unwrap().as_object().unwrap().clone();
        let props = wire.properties.as_object().unwrap().clone();
        (options, props)
    }

    #[test]
    fn options_bool_coercion() {
        // (input value, expected wire bool or None when omitted). Covers native
        // bool, the string/numeric forms the backend tolerates, and
        // uninterpretable values that must be dropped (not shipped mistyped).
        let cases: [(serde_json::Value, Option<bool>); 13] = [
            (serde_json::json!(true), Some(true)),
            (serde_json::json!(false), Some(false)),
            (serde_json::json!("true"), Some(true)),
            (serde_json::json!("false"), Some(false)),
            (serde_json::json!("TRUE"), Some(true)),
            (serde_json::json!("  True  "), Some(true)),
            (serde_json::json!("1"), Some(true)),
            (serde_json::json!("0"), Some(false)),
            (serde_json::json!(1), Some(true)),
            (serde_json::json!(0), Some(false)),
            (serde_json::json!(2), Some(true)),
            (serde_json::json!("yes"), None),
            (serde_json::json!(null), None),
        ];
        // Assert one representative bool key end-to-end, then confirm every bool
        // key in the table shares the path with a single non-coercible value.
        for (input, expected) in cases {
            let (options, props) = lift_one("$cookieless_mode", input.clone());
            assert_eq!(
                options.get("cookieless_mode"),
                expected.map(serde_json::Value::Bool).as_ref(),
                "cookieless_mode input={:?}",
                input
            );
            // Always stripped from properties, even when coercion fails.
            assert!(
                !props.contains_key("$cookieless_mode"),
                "magic key must be stripped, input={:?}",
                input
            );
        }

        // All three bool option keys behave identically: a non-coercible value
        // is dropped, a coercible one is normalized — and either way the magic
        // key is stripped from properties (it must never reach the backend props).
        for (prop_key, wire_key) in [
            ("$cookieless_mode", "cookieless_mode"),
            ("$ignore_sent_at", "disable_skew_correction"),
            ("$process_person_profile", "process_person_profile"),
        ] {
            let (bad, bad_props) = lift_one(prop_key, serde_json::json!(["nope"]));
            assert!(
                !bad.contains_key(wire_key),
                "{}: array must be dropped",
                wire_key
            );
            assert!(
                !bad_props.contains_key(prop_key),
                "{}: magic key must be stripped even when coercion fails",
                prop_key
            );
            let (good, good_props) = lift_one(prop_key, serde_json::json!("1"));
            assert_eq!(
                good.get(wire_key),
                Some(&serde_json::json!(true)),
                "{}: \"1\" must coerce to true",
                wire_key
            );
            assert!(
                !good_props.contains_key(prop_key),
                "{}: magic key must be stripped on coercion success",
                prop_key
            );
        }
    }

    #[test]
    fn options_product_tour_id_coercion() {
        // product_tour_id is Option<String>: only strings pass; other JSON
        // types are dropped so the backend doesn't 400 on the batch.
        let cases: [(serde_json::Value, Option<&str>); 5] = [
            (serde_json::json!("tour-42"), Some("tour-42")),
            (serde_json::json!(""), Some("")),
            (serde_json::json!(42), None),
            (serde_json::json!(true), None),
            (serde_json::json!(["a"]), None),
        ];
        for (input, expected) in cases {
            let (options, props) = lift_one("$product_tour_id", input.clone());
            assert_eq!(
                options.get("product_tour_id"),
                expected
                    .map(|s| serde_json::Value::String(s.to_string()))
                    .as_ref(),
                "product_tour_id input={:?}",
                input
            );
            assert!(
                !props.contains_key("$product_tour_id"),
                "magic key must be stripped, input={:?}",
                input
            );
        }
    }

    // -- session/window extraction -------------------------------------------

    #[test]
    fn event_extracts_session_window_from_properties() {
        let mut event = Event::new("test", "user-1");
        event.insert_prop("$session_id", "sess-123").unwrap();
        event.insert_prop("$window_id", "win-456").unwrap();

        let wire = CaptureEvent::from_event(&event);

        assert_eq!(wire.session_id, Some("sess-123".to_string()));
        assert_eq!(wire.window_id, Some("win-456".to_string()));
        let props = wire.properties.as_object().unwrap();
        assert!(!props.contains_key("$session_id"));
        assert!(!props.contains_key("$window_id"));
    }

    // -- groups --------------------------------------------------------------

    #[test]
    fn event_groups_in_properties() {
        let mut event = Event::new("test", "user-1");
        event.add_group("company", "acme");

        let wire = CaptureEvent::from_event(&event);

        let props = wire.properties.as_object().unwrap();
        let groups = props.get("$groups").unwrap().as_object().unwrap();
        assert_eq!(groups.get("company").unwrap().as_str().unwrap(), "acme");
        // add_group forces process_person_profile=true.
        let json = serde_json::to_value(&wire).unwrap();
        let options = json.get("options").unwrap().as_object().unwrap();
        assert_eq!(
            options.get("process_person_profile"),
            Some(&serde_json::json!(true))
        );
    }

    // -- event root fields ----------------------------------------------------

    #[test]
    fn serializes_distinct_id_at_root() {
        let json =
            serde_json::to_value(CaptureEvent::from_event(&Event::new("test", "user1"))).unwrap();

        // Canonical field at the event root; the legacy `$distinct_id` spelling
        // (only tolerated by capture via a serde alias) must not be emitted, and
        // it must not be duplicated into properties.
        assert_eq!(json["distinct_id"], "user1");
        assert!(json.get("$distinct_id").is_none());
        assert!(json["properties"].get("distinct_id").is_none());
    }

    #[test]
    fn includes_auto_generated_uuid() {
        let json =
            serde_json::to_value(CaptureEvent::from_event(&Event::new("test", "user1"))).unwrap();

        let uuid_str = json["uuid"].as_str().expect("uuid should be present");
        Uuid::parse_str(uuid_str).expect("uuid should be valid");
    }

    #[test]
    fn preserves_overridden_uuid() {
        let uuid = Uuid::now_v7();
        let mut event = Event::new("test", "user1");
        event.set_uuid(uuid);

        let wire = CaptureEvent::from_event(&event);
        assert_eq!(wire.uuid, uuid);
    }

    #[test]
    fn no_process_person_profile_when_unset() {
        let wire = CaptureEvent::from_event(&Event::new("test", "user1"));
        let json = serde_json::to_value(&wire).unwrap();

        // Absent everywhere: not defaulted into options, not left in properties.
        assert!(json["options"].get("process_person_profile").is_none());
        assert!(json["properties"].get("$process_person_profile").is_none());
    }

    // -- batch / response serialization (unchanged) --------------------------

    #[test]
    fn batch_request_serializes() {
        let event = Event::new("test", "user-1");
        let batch = BatchRequest {
            created_at: "2026-05-28T15:00:00Z".to_string(),
            historical_migration: None,
            batch: vec![CaptureEvent::from_event(&event)],
        };

        let json = serde_json::to_value(&batch).unwrap();
        assert_eq!(json["created_at"], "2026-05-28T15:00:00Z");
        assert!(json.get("historical_migration").is_none());
        assert_eq!(json["batch"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn batch_response_deserializes() {
        let json = r#"{
            "results": {
                "550e8400-e29b-41d4-a716-446655440000": {"result": "ok"},
                "550e8400-e29b-41d4-a716-446655440001": {"result": "retry", "details": "not_persisted"},
                "550e8400-e29b-41d4-a716-446655440002": {"result": "drop", "details": "billing_limit_exceeded"}
            }
        }"#;

        let u0 = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let u1 = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap();

        let resp: CaptureResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.results.len(), 3);
        assert_eq!(resp.results[&u0].result, EventStatus::Ok);
        assert_eq!(resp.results[&u1].result, EventStatus::Retry);
        assert_eq!(resp.results[&u1].details, Some("not_persisted".to_string()));
    }

    #[test]
    fn warning_status_deserializes_as_warning() {
        let json = r#"{
            "results": {
                "550e8400-e29b-41d4-a716-446655440000": {"result": "warning", "details": "person_processing_disabled"}
            }
        }"#;

        let u = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let resp: CaptureResponse = serde_json::from_str(json).unwrap();
        let entry = &resp.results[&u];
        assert_eq!(entry.result, EventStatus::Warning);
        assert_eq!(
            entry.details,
            Some("person_processing_disabled".to_string())
        );
    }

    #[test]
    fn unknown_status_deserializes_as_unknown() {
        let json = r#"{
            "results": {
                "550e8400-e29b-41d4-a716-446655440000": {"result": "ok"},
                "550e8400-e29b-41d4-a716-446655440001": {"result": "some_future_status", "details": "new_detail"}
            }
        }"#;

        let u1 = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap();
        let resp: CaptureResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.results.len(), 2);
        assert_eq!(resp.results[&u1].result, EventStatus::Unknown);
    }

    #[test]
    fn limited_status_deserializes_as_unknown() {
        let json = r#"{
            "results": {
                "550e8400-e29b-41d4-a716-446655440000": {"result": "limited"}
            }
        }"#;

        let u = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let resp: CaptureResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.results[&u].result, EventStatus::Unknown);
    }

    #[test]
    fn error_response_deserializes() {
        let json = r#"{
            "error": "billing_limit_exceeded",
            "error_description": "Billing quota exceeded.",
            "error_uri": "https://posthog.com/docs/billing/limits"
        }"#;

        let err: CaptureErrorResponse = serde_json::from_str(json).unwrap();
        assert_eq!(err.error, "billing_limit_exceeded");
        assert_eq!(
            err.error_description,
            Some("Billing quota exceeded.".to_string())
        );
    }

    // -- unknown properties are NOT lifted -----------------------------------

    #[test]
    fn unknown_properties_stay_in_properties() {
        let mut event = Event::new("test", "user-1");
        event.insert_prop("$cookieless_mode", true).unwrap();
        event.insert_prop("custom_metric", 42).unwrap();
        event.insert_prop("future_backend_flag", "hello").unwrap();

        let wire = CaptureEvent::from_event(&event);
        let json = serde_json::to_value(&wire).unwrap();
        let options = json.get("options").unwrap().as_object().unwrap();

        // Known key extracted.
        assert_eq!(
            options.get("cookieless_mode"),
            Some(&serde_json::json!(true))
        );
        // Unknown keys NOT lifted — stay in properties.
        assert!(!options.contains_key("custom_metric"));
        assert!(!options.contains_key("future_backend_flag"));
        let props = wire.properties.as_object().unwrap();
        assert_eq!(props.get("custom_metric"), Some(&serde_json::json!(42)));
        assert_eq!(
            props.get("future_backend_flag"),
            Some(&serde_json::json!("hello"))
        );
    }

    // -- empty options map renders as {} on the wire -------------------------

    #[test]
    fn empty_options_serializes_as_empty_object() {
        let event = Event::new("test", "user-1");
        let wire = CaptureEvent::from_event(&event);
        let json_str = serde_json::to_string(&wire).unwrap();
        assert!(json_str.contains("\"options\":{}"));
    }

    // -- anon event: property extracted, not in properties -------------------

    #[test]
    fn anon_event_process_person_profile_in_options_not_properties() {
        let event = Event::new_anon("test");
        let wire = CaptureEvent::from_event(&event);
        let json = serde_json::to_value(&wire).unwrap();
        let options = json.get("options").unwrap().as_object().unwrap();
        assert_eq!(
            options.get("process_person_profile"),
            Some(&serde_json::json!(false))
        );
        let props = wire.properties.as_object().unwrap();
        assert!(!props.contains_key("$process_person_profile"));
    }

    // -- explicit insert_prop wins over constructor default ------------------

    #[test]
    fn explicit_insert_prop_wins_over_anon_default() {
        let mut event = Event::new_anon("test");
        // new_anon sets $process_person_profile=false; explicit insert overwrites.
        event.insert_prop("$process_person_profile", true).unwrap();
        let wire = CaptureEvent::from_event(&event);
        let json = serde_json::to_value(&wire).unwrap();
        let options = json.get("options").unwrap().as_object().unwrap();
        assert_eq!(
            options.get("process_person_profile"),
            Some(&serde_json::json!(true))
        );
        let props = wire.properties.as_object().unwrap();
        assert!(!props.contains_key("$process_person_profile"));
    }

    #[test]
    fn identified_event_with_explicit_personless() {
        let mut event = Event::new("test", "user-1");
        event.insert_prop("$process_person_profile", false).unwrap();
        let wire = CaptureEvent::from_event(&event);
        let json = serde_json::to_value(&wire).unwrap();
        let options = json.get("options").unwrap().as_object().unwrap();
        assert_eq!(
            options.get("process_person_profile"),
            Some(&serde_json::json!(false))
        );
        let props = wire.properties.as_object().unwrap();
        assert!(!props.contains_key("$process_person_profile"));
    }

    #[test]
    fn add_group_overrides_anon_person_profile() {
        let mut event = Event::new_anon("test");
        // new_anon sets $process_person_profile=false; add_group forces true.
        event.add_group("company", "acme");
        let wire = CaptureEvent::from_event(&event);
        let json = serde_json::to_value(&wire).unwrap();
        let options = json.get("options").unwrap().as_object().unwrap();
        assert_eq!(
            options.get("process_person_profile"),
            Some(&serde_json::json!(true))
        );
        let props = wire.properties.as_object().unwrap();
        assert!(!props.contains_key("$process_person_profile"));
        let groups = props.get("$groups").unwrap().as_object().unwrap();
        assert_eq!(groups.get("company").unwrap().as_str().unwrap(), "acme");
    }
}

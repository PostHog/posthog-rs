use std::collections::HashMap;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event::{Event, EventOptions};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1Event {
    pub event: String,
    pub uuid: Uuid,
    pub distinct_id: String,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    pub options: EventOptions,
    pub properties: serde_json::Value,
}

impl V1Event {
    pub fn from_event(event: &Event) -> Self {
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
            .unwrap_or_else(|| Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string());

        // V1 carries process_person_profile in options; strip the property
        // duplicate in case a caller manually inserted it.
        if event.options().process_person_profile.is_some() {
            properties.remove("$process_person_profile");
        }

        let session_id = properties
            .remove("$session_id")
            .and_then(|v| v.as_str().map(String::from));
        let window_id = properties
            .remove("$window_id")
            .and_then(|v| v.as_str().map(String::from));

        Self {
            event: event.event_name().to_string(),
            uuid: event.uuid(),
            distinct_id: event.distinct_id().to_string(),
            timestamp,
            session_id,
            window_id,
            options: event.options().clone(),
            properties: serde_json::to_value(properties)
                .unwrap_or(serde_json::Value::Object(Default::default())),
        }
    }
}

/// Owned variant used by tests; the capture pipeline uses [`V1BatchRequestRef`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct V1BatchRequest {
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub historical_migration: Option<bool>,
    pub batch: Vec<V1Event>,
}

/// Serialize-only borrowed twin of [`V1BatchRequest`]; avoids per-attempt clones.
#[derive(Debug, Serialize)]
pub(crate) struct V1BatchRequestRef<'a> {
    pub created_at: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub historical_migration: Option<bool>,
    pub batch: &'a [V1Event],
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
pub struct V1ErrorResponse {
    pub error: String,
    #[serde(default)]
    pub error_description: Option<String>,
    #[serde(default)]
    pub error_uri: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Event;

    #[test]
    fn v1_event_from_event_basic() {
        let event = Event::new("test_event", "user-1");
        let v1 = V1Event::from_event(&event);

        assert_eq!(v1.event, "test_event");
        assert_eq!(v1.distinct_id, "user-1");
        assert_eq!(v1.options.process_person_profile, None);
        assert_eq!(v1.options.cookieless_mode, None);
        assert_eq!(v1.options.disable_skew_correction, None);
        assert!(v1.session_id.is_none());
        assert!(v1.window_id.is_none());
    }

    #[test]
    fn v1_event_from_event_anon() {
        let event = Event::new_anon("anon_event");
        let v1 = V1Event::from_event(&event);

        assert_eq!(v1.event, "anon_event");
        assert_eq!(v1.options.process_person_profile, Some(false));
        let props = v1.properties.as_object().unwrap();
        assert!(!props.contains_key("$process_person_profile"));
    }

    #[test]
    fn v1_event_options_overrides_serialize_and_unset_are_omitted() {
        let mut event = Event::new("test_event", "user-1");
        event.set_option("cookieless_mode", true).unwrap();
        event.set_option("process_person_profile", false).unwrap();
        event.set_option("product_tour_id", "tour-42").unwrap();

        let v1 = V1Event::from_event(&event);
        let json = serde_json::to_value(&v1).unwrap();
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
        assert!(!options.contains_key("disable_skew_correction"));
    }

    #[test]
    fn v1_event_extracts_session_window_from_properties() {
        let mut event = Event::new("test", "user-1");
        event.insert_prop("$session_id", "sess-123").unwrap();
        event.insert_prop("$window_id", "win-456").unwrap();

        let v1 = V1Event::from_event(&event);

        assert_eq!(v1.session_id, Some("sess-123".to_string()));
        assert_eq!(v1.window_id, Some("win-456".to_string()));
        let props = v1.properties.as_object().unwrap();
        assert!(!props.contains_key("$session_id"));
        assert!(!props.contains_key("$window_id"));
    }

    #[test]
    fn v1_event_groups_in_properties() {
        let mut event = Event::new("test", "user-1");
        event.add_group("company", "acme");

        let v1 = V1Event::from_event(&event);

        let props = v1.properties.as_object().unwrap();
        let groups = props.get("$groups").unwrap().as_object().unwrap();
        assert_eq!(groups.get("company").unwrap().as_str().unwrap(), "acme");
        assert_eq!(v1.options.process_person_profile, Some(true));
    }

    #[test]
    fn v1_batch_request_serializes() {
        let event = Event::new("test", "user-1");
        let batch = V1BatchRequest {
            created_at: "2026-05-28T15:00:00Z".to_string(),
            historical_migration: None,
            batch: vec![V1Event::from_event(&event)],
        };

        let json = serde_json::to_value(&batch).unwrap();
        assert_eq!(json["created_at"], "2026-05-28T15:00:00Z");
        assert!(json.get("historical_migration").is_none());
        assert_eq!(json["batch"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn v1_batch_response_deserializes() {
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
    fn v1_warning_status_deserializes_as_warning() {
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
    fn v1_unknown_status_deserializes_as_unknown() {
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
    fn v1_limited_status_deserializes_as_unknown() {
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
    fn v1_error_response_deserializes() {
        let json = r#"{
            "error": "billing_limit_exceeded",
            "error_description": "Billing quota exceeded.",
            "error_uri": "https://posthog.com/docs/billing/limits"
        }"#;

        let err: V1ErrorResponse = serde_json::from_str(json).unwrap();
        assert_eq!(err.error, "billing_limit_exceeded");
        assert_eq!(
            err.error_description,
            Some("Billing quota exceeded.".to_string())
        );
    }

    #[test]
    fn v1_extra_options_serialize_as_top_level_keys() {
        let mut event = Event::new("test", "user-1");
        event.set_option("cookieless_mode", true).unwrap();
        event.set_option("future_flag", true).unwrap();
        event.set_option("routing_key", "us-east").unwrap();

        let v1 = V1Event::from_event(&event);
        let json = serde_json::to_value(&v1).unwrap();
        let options = json.get("options").unwrap().as_object().unwrap();

        assert_eq!(
            options.get("cookieless_mode"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(options.get("future_flag"), Some(&serde_json::json!(true)));
        assert_eq!(
            options.get("routing_key"),
            Some(&serde_json::json!("us-east"))
        );
        assert!(!options.contains_key("disable_skew_correction"));
    }

    #[test]
    fn v1_extra_options_empty_map_omitted_from_wire() {
        let event = Event::new("test", "user-1");
        let v1 = V1Event::from_event(&event);
        let json_str = serde_json::to_string(&v1).unwrap();
        assert!(!json_str.contains("extra"));
    }

    #[test]
    fn v1_set_option_routes_unknown_keys_to_extra() {
        let mut event = Event::new("test", "user-1");
        event.set_option("new_backend_flag", true).unwrap();
        event.set_option("batch_priority", 5u32).unwrap();

        let v1 = V1Event::from_event(&event);
        let json = serde_json::to_value(&v1).unwrap();
        let options = json.get("options").unwrap().as_object().unwrap();

        assert_eq!(
            options.get("new_backend_flag"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(options.get("batch_priority"), Some(&serde_json::json!(5)));
    }

    #[test]
    fn v1_anon_event_no_process_person_profile_in_properties() {
        let event = Event::new_anon("test");
        let v1 = V1Event::from_event(&event);
        assert_eq!(v1.options.process_person_profile, Some(false));
        let props = v1.properties.as_object().unwrap();
        assert!(!props.contains_key("$process_person_profile"));
    }

    #[test]
    fn v1_strips_duplicate_process_person_profile_property() {
        let mut event = Event::new_anon("test");
        event.insert_prop("$process_person_profile", false).unwrap();
        let v1 = V1Event::from_event(&event);
        assert_eq!(v1.options.process_person_profile, Some(false));
        let props = v1.properties.as_object().unwrap();
        assert!(!props.contains_key("$process_person_profile"));
    }
}

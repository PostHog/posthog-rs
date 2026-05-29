use std::collections::HashMap;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event::{Event, EventOptions};

/// A single event in the V1 wire format.
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
    /// Convert a user-facing `Event` into the V1 wire format.
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

/// The batch request body for V1 capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1BatchRequest {
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub historical_migration: Option<bool>,
    pub batch: Vec<V1Event>,
}

/// Per-event result status returned by the V1 endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum V1EventStatus {
    Ok,
    Drop,
    Limited,
    Retry,
}

/// Per-event result entry in the V1 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1EventResult {
    pub result: V1EventStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// The V1 batch response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1BatchResponse {
    pub results: HashMap<String, V1EventResult>,
}

/// Structured error response for non-2xx V1 responses.
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
        assert!(v1.options.process_person_profile);
        assert!(!v1.options.cookieless_mode);
        assert!(v1.session_id.is_none());
        assert!(v1.window_id.is_none());
    }

    #[test]
    fn v1_event_from_event_anon() {
        let event = Event::new_anon("anon_event");
        let v1 = V1Event::from_event(&event);

        assert_eq!(v1.event, "anon_event");
        assert!(!v1.options.process_person_profile);
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
        assert!(v1.options.process_person_profile);
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

        let resp: V1BatchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.results.len(), 3);
        assert_eq!(
            resp.results["550e8400-e29b-41d4-a716-446655440000"].result,
            V1EventStatus::Ok
        );
        assert_eq!(
            resp.results["550e8400-e29b-41d4-a716-446655440001"].result,
            V1EventStatus::Retry
        );
        assert_eq!(
            resp.results["550e8400-e29b-41d4-a716-446655440001"].details,
            Some("not_persisted".to_string())
        );
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
}

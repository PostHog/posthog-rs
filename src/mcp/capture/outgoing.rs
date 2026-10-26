use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use super::context::EventContext;
use crate::mcp::bounds::{bounded_payload, fit_event_size};
use crate::mcp::contract::properties as key;
use crate::mcp::contract::{INTERFACE, LIB_NAME};
use crate::mcp::privacy::TextPass;
use crate::Event;

const MCP_PREFIX: &str = "$mcp_";
const PERSON_CONTROLS: [&str; 5] = [
    key::PROCESS_PERSON_PROFILE,
    key::SET,
    "$set_once",
    "$unset",
    key::GROUPS,
];

const ALWAYS_REQUIRED: [&str; 8] = [
    key::INTERFACE,
    key::SERVER_BUILD,
    key::LIB,
    key::SESSION_ID,
    key::PROCESS_PERSON_PROFILE,
    key::EXCEPTION_LIST,
    key::EXCEPTION_LEVEL,
    key::EXCEPTION_SOURCE,
];

/// One PostHog event an MCP request produces.
#[derive(Clone, Debug)]
pub(crate) struct OutgoingEvent {
    pub(crate) name: &'static str,
    distinct_id: String,
    timestamp: DateTime<Utc>,
    pub(crate) properties: Map<String, Value>,
}

impl OutgoingEvent {
    pub(crate) fn new(name: &'static str, context: &EventContext) -> Self {
        let mut properties = context.properties();
        properties.insert(key::INTERFACE.to_string(), Value::from(INTERFACE));
        properties.insert(key::LIB.to_string(), Value::from(LIB_NAME));
        add_host_properties(&mut properties, &context.host_properties);
        Self {
            name,
            distinct_id: context.distinct_id().to_string(),
            timestamp: context.timestamp,
            properties,
        }
    }

    pub(crate) fn set(&mut self, key: &str, value: impl Into<Value>) {
        self.properties.insert(key.to_string(), value.into());
    }

    pub(crate) fn set_optional(&mut self, key: &str, value: Option<impl Into<Value>>) {
        if let Some(value) = value {
            self.set(key, value);
        }
    }

    pub(crate) fn extend(&mut self, properties: Map<String, Value>) {
        self.properties.extend(properties);
    }

    /// Fits the event within the byte bound, keeping `required` and the
    /// properties every MCP event needs.
    pub(crate) fn bounded(mut self, required: &[&str]) -> Self {
        let mut kept: Vec<&str> = ALWAYS_REQUIRED.to_vec();
        kept.extend_from_slice(required);
        fit_event_size(&mut self.properties, &kept);
        self
    }

    pub(crate) fn into_posthog_event(self) -> Event {
        let mut event = Event::new(self.name.to_string(), self.distinct_id);
        for (name, value) in self.properties {
            let _ = event.insert_prop(name, value);
        }
        let _ = event.set_timestamp(self.timestamp);
        event.request_context_applied = true;
        event
    }
}

/// The host's request context fills in what the event lacks, here rather
/// than at capture, so the size bound and redaction cover it. The MCP
/// session and person win over the host's: the person controls and the
/// `$mcp_*` names are MCP analytics' alone.
fn add_host_properties(properties: &mut Map<String, Value>, host: &Map<String, Value>) {
    let kept: Map<String, Value> = host
        .iter()
        .filter(|(name, _)| {
            !name.starts_with(MCP_PREFIX) && !PERSON_CONTROLS.contains(&name.as_str())
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    if let Value::Object(redacted) = bounded_payload(&Value::Object(kept), TextPass::Name) {
        for (name, value) in redacted {
            properties.entry(name).or_insert(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::mcp::contract::events::TOOLS_LIST;
    use crate::mcp::contract::properties::LISTED_TOOL_NAMES;

    fn context() -> EventContext {
        EventContext {
            timestamp: Utc::now(),
            session_id: "ses_1".to_string(),
            conversation_id: None,
            identity: None,
            client: Default::default(),
            server: Default::default(),
            server_build: None,
            host_properties: Map::new(),
        }
    }

    #[test]
    fn an_oversized_event_keeps_its_interface() {
        let context = context();
        let names: Vec<String> = (0..3_000).map(|index| format!("{index:058}")).collect();
        let mut event = OutgoingEvent::new(TOOLS_LIST, &context);
        event.set(LISTED_TOOL_NAMES, names);
        let event = event.bounded(&[LISTED_TOOL_NAMES]);
        assert_eq!(event.properties[key::INTERFACE], INTERFACE);
    }

    #[test]
    fn host_properties_are_bounded_with_the_event() {
        let mut context = context();
        context.host_properties = serde_json::json!({
            "$session_id": "ses_host",
            "tenant": "t".repeat(200_000),
        })
        .as_object()
        .cloned()
        .unwrap();
        let event = OutgoingEvent::new(TOOLS_LIST, &context).bounded(&[]);

        assert_eq!(event.properties[key::SESSION_ID], "ses_1");
        assert!(event.properties.contains_key("tenant"));
        let size = serde_json::to_vec(&event.properties).unwrap().len();
        let limits = crate::mcp::contract::limits::MAX_EVENT_BYTES
            - crate::mcp::contract::limits::HOST_PROPERTIES_HEADROOM;
        assert!(size <= limits, "{size}");
        assert!(event.into_posthog_event().request_context_applied);
    }

    #[test]
    fn host_properties_are_redacted_and_leave_identity_to_mcp() {
        let mut context = context();
        context.identity = Some(crate::mcp::Identity::new("user-1").with_property("plan", "pro"));
        context.host_properties = serde_json::json!({
            "authorization": "Bearer secret",
            "nested": { "password": "hunter2", "region": "eu" },
            "$process_person_profile": false,
            "$set": { "plan": "free" },
            "$groups": { "company": "other" },
            "$mcp_intent": "host text",
        })
        .as_object()
        .cloned()
        .unwrap();

        let event = OutgoingEvent::new(TOOLS_LIST, &context);

        let properties = &event.properties;
        assert_eq!(properties["authorization"], "[redacted]");
        assert_eq!(
            properties["nested"],
            serde_json::json!({ "password": "[redacted]", "region": "eu" })
        );
        assert!(!properties.contains_key(key::PROCESS_PERSON_PROFILE));
        assert_eq!(properties[key::SET], serde_json::json!({ "plan": "pro" }));
        assert!(!properties.contains_key(key::GROUPS));
        assert!(!properties.contains_key("$mcp_intent"));
    }
}

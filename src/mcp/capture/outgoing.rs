use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use super::context::EventContext;
use crate::mcp::bounds::fit_event_size;
use crate::mcp::contract::properties as key;
use crate::mcp::contract::{INTERFACE, LIB_NAME};
use crate::Event;

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
    host_properties: Map<String, Value>,
}

impl OutgoingEvent {
    pub(crate) fn new(name: &'static str, context: &EventContext) -> Self {
        let mut properties = context.properties();
        properties.insert(key::INTERFACE.to_string(), Value::from(INTERFACE));
        properties.insert(key::LIB.to_string(), Value::from(LIB_NAME));
        Self {
            name,
            distinct_id: context.distinct_id().to_string(),
            timestamp: context.timestamp,
            properties,
            host_properties: context.host_properties.clone(),
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
    /// properties every MCP event needs. The host's request context fills in
    /// what the event lacks, here rather than at capture, so the size bound and
    /// redaction cover it. It takes only the room MCP's own properties leave,
    /// and never a name MCP set.
    pub(crate) fn bounded(mut self, required: &[&str]) -> Self {
        let mut kept: Vec<&str> = ALWAYS_REQUIRED.to_vec();
        kept.extend_from_slice(required);
        fit_event_size(&mut self.properties, &kept, &self.host_properties);
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

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::mcp::capture::host_properties;
    use crate::mcp::contract::events::TOOLS_LIST;
    use crate::mcp::contract::limits::MAX_PAYLOAD_STRING_LENGTH;
    use crate::mcp::contract::properties::LISTED_TOOL_NAMES;
    use crate::RequestContext;

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
        let host = RequestContext::new()
            .session_id("ses_host")
            .property("tenant", "t".repeat(200_000))
            .unwrap();
        context.host_properties = host_properties(&host);
        let event = OutgoingEvent::new(TOOLS_LIST, &context).bounded(&[]);

        assert_eq!(event.properties[key::SESSION_ID], "ses_1");
        assert!(event.properties.contains_key("tenant"));
        let size = serde_json::to_vec(&event.properties).unwrap().len();
        let limits = crate::mcp::contract::limits::MAX_EVENT_BYTES
            - crate::mcp::contract::limits::HOST_PROPERTIES_HEADROOM;
        assert!(size <= limits, "{}", size);
        assert!(event.into_posthog_event().request_context_applied);
    }

    #[test]
    fn host_properties_are_redacted_and_leave_identity_to_mcp() {
        let mut context = context();
        context.identity = Some(crate::mcp::Identity::new("user-1").with_property("plan", "pro"));
        let host = serde_json::json!({
            "authorization": "Bearer secret",
            "nested": { "password": "hunter2", "region": "eu" },
            "$process_person_profile": false,
            "$set": { "plan": "free" },
            "$groups": { "company": "other" },
            "$mcp_intent": "host text",
        });
        let host = host
            .as_object()
            .unwrap()
            .iter()
            .fold(RequestContext::new(), |host, (name, value)| {
                host.property(name.as_str(), value).unwrap()
            });
        context.host_properties = host_properties(&host);

        let event = OutgoingEvent::new(TOOLS_LIST, &context).bounded(&[]);

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

    #[test]
    fn host_properties_keep_every_name_and_bound_each_value() {
        let host = (0..150).fold(RequestContext::new(), |host, index| {
            host.property(format!("p{index:03}"), "small").unwrap()
        });
        let host = host.property("zz_tenant", "word ".repeat(20_000)).unwrap();

        let kept = host_properties(&host);

        assert_eq!(kept.len(), 151);
        assert!(!kept.contains_key("..."));
        let tenant = kept["zz_tenant"].as_str().unwrap();
        assert_eq!(tenant.len(), MAX_PAYLOAD_STRING_LENGTH);
    }

    #[test]
    fn host_properties_give_way_to_mcp_payloads() {
        let mut context = context();
        let host = (0..4_000).fold(RequestContext::new(), |host, index| {
            host.property(format!("k{index:04}"), "v".repeat(40))
                .unwrap()
        });
        context.host_properties = host_properties(&host);
        let mut event = OutgoingEvent::new(TOOLS_LIST, &context);
        event.set(key::PARAMETERS, serde_json::json!({ "q": "hello" }));
        event.set(key::RESPONSE, serde_json::json!({ "a": "world" }));

        let event = event.bounded(&[]);

        assert_eq!(
            event.properties[key::PARAMETERS],
            serde_json::json!({ "q": "hello" })
        );
        assert_eq!(
            event.properties[key::RESPONSE],
            serde_json::json!({ "a": "world" })
        );
        assert!(event.properties.contains_key("k0000"));
        let size = serde_json::to_vec(&event.properties).unwrap().len();
        let limit = crate::mcp::contract::limits::MAX_EVENT_BYTES
            - crate::mcp::contract::limits::HOST_PROPERTIES_HEADROOM;
        assert!(size <= limit, "{}", size);
    }

    #[test]
    fn a_call_keeps_no_more_host_properties_than_one_event_holds() {
        let host = (0..20_000).fold(RequestContext::new(), |host, index| {
            host.property(format!("k{index:05}"), "v".repeat(40))
                .unwrap()
        });

        let kept = host_properties(&host);

        assert!(kept.contains_key("k00000"));
        assert!(kept.len() < 20_000, "{}", kept.len());
        let size = serde_json::to_vec(&kept).unwrap().len();
        assert!(
            size <= crate::mcp::contract::limits::MAX_EVENT_BYTES,
            "{}",
            size
        );
    }
}

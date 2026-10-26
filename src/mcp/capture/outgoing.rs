use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use super::context::EventContext;
use crate::mcp::bounds::fit_event_size;
use crate::mcp::contract::properties as key;
use crate::mcp::contract::{INTERFACE, LIB_NAME};
use crate::{Event, RequestContext};

const ALWAYS_REQUIRED: [&str; 7] = [
    key::INTERFACE,
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
        add_host_request_context(&mut properties);
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
/// than at capture, so the size bound covers it. The MCP session and person
/// win over the host's.
fn add_host_request_context(properties: &mut Map<String, Value>) {
    let Some(host) = RequestContext::current() else {
        return;
    };
    for (name, value) in host.properties {
        properties.entry(name).or_insert(value);
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

    #[tokio::test]
    async fn host_request_context_is_bounded_with_the_event() {
        let host = crate::RequestContext::new()
            .session_id("ses_host")
            .property("tenant", "t".repeat(200_000))
            .unwrap();
        let event = host
            .scope(async { OutgoingEvent::new(TOOLS_LIST, &context()).bounded(&[]) })
            .await;

        assert_eq!(event.properties[key::SESSION_ID], "ses_1");
        assert!(event.properties.contains_key("tenant"));
        let size = serde_json::to_vec(&event.properties).unwrap().len();
        assert!(
            size <= crate::mcp::contract::limits::MAX_EVENT_BYTES,
            "{size}"
        );
        assert!(event.into_posthog_event().request_context_applied);
    }
}

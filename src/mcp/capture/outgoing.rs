use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use super::context::EventContext;
use crate::client::CRATE_VERSION;
use crate::constants::PROCESS_PERSON_PROFILE_OPT;
use crate::mcp::bounds::fit_event_size;
use crate::mcp::contract::properties as key;
use crate::mcp::contract::{INTERFACE, LIB_NAME};
use crate::{Event, RequestContext};

const ALWAYS_REQUIRED: [&str; 7] = [
    key::INTERFACE,
    key::LIB,
    key::LIB_VERSION,
    key::SESSION_ID,
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
    /// Set for anonymous calls; sent as an event option so a request context option cannot
    /// turn person processing back on.
    personless: bool,
    pub(crate) properties: Map<String, Value>,
    request_context: Option<RequestContext>,
}

impl OutgoingEvent {
    pub(crate) fn new(name: &'static str, context: &EventContext) -> Self {
        let mut properties = context.properties();
        properties.insert(key::INTERFACE.to_string(), Value::from(INTERFACE));
        label_library(&mut properties);
        Self {
            name,
            distinct_id: context.distinct_id().to_string(),
            timestamp: context.timestamp,
            personless: context.identity.is_none(),
            properties,
            request_context: context.request_context.clone(),
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
        if self.personless {
            let _ = event.insert_option(PROCESS_PERSON_PROFILE_OPT, false);
        }
        let _ = event.set_timestamp(self.timestamp);
        // Applied before the MCP `before_send` rather than at enqueue, so what the hook returns is sent.
        if let Some(context) = RequestContext::current().or(self.request_context) {
            context.apply_to_event(&mut event);
        }
        event
    }
}

/// Names MCP analytics as the library, as a pair so the event never carries
/// `posthog-rs-mcp` without a version.
fn label_library(properties: &mut Map<String, Value>) {
    properties.insert(key::LIB.to_string(), Value::from(LIB_NAME));
    properties.insert(key::LIB_VERSION.to_string(), Value::from(CRATE_VERSION));
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use serde_json::json;

    use super::*;
    use crate::constants::PROCESS_PERSON_PROFILE_OPT;
    use crate::mcp::contract::events::{TOOLS_LIST, TOOL_CALL};
    use crate::mcp::contract::properties::LISTED_TOOL_NAMES;
    use crate::mcp::Identity;
    use crate::RequestContext;

    fn context(identity: Option<Identity>) -> EventContext {
        EventContext {
            timestamp: Utc::now(),
            session_id: "ses_1".to_string(),
            conversation_id: None,
            identity,
            client: Default::default(),
            server: Default::default(),
            request_context: None,
        }
    }

    fn person_processing(event: &Event) -> Option<&Value> {
        event.options().get(PROCESS_PERSON_PROFILE_OPT)
    }

    fn request(option: bool) -> RequestContext {
        RequestContext::new()
            .distinct_id("context-user")
            .property("route", "/mcp")
            .unwrap()
            .option(PROCESS_PERSON_PROFILE_OPT, option)
            .unwrap()
    }

    fn converted(identity: Option<Identity>, request: Option<RequestContext>) -> Event {
        let mut context = context(identity);
        context.request_context = request;
        OutgoingEvent::new(TOOL_CALL, &context).into_posthog_event()
    }

    #[test]
    fn an_anonymous_event_stays_personless_under_a_request_context() {
        let event = converted(None, Some(request(true)));
        assert_eq!(person_processing(&event), Some(&json!(false)));
        assert_eq!(event.distinct_id(), "ses_1");
        assert_eq!(event.properties()["route"], "/mcp");
        assert!(!event.properties().contains_key("$process_person_profile"));
    }

    #[test]
    fn an_identified_event_keeps_its_identity_and_takes_the_context_options() {
        let event = converted(Some(Identity::new("user-1")), Some(request(false)));
        assert_eq!(event.distinct_id(), "user-1");
        assert_eq!(person_processing(&event), Some(&json!(false)));
        assert_eq!(event.properties()["route"], "/mcp");
    }

    #[tokio::test]
    async fn a_scoped_context_wins_over_the_http_requests() {
        let scoped = RequestContext::new().property("route", "/scoped").unwrap();
        let event = scoped
            .scope(async { converted(None, Some(request(true))) })
            .await;
        assert_eq!(event.properties()["route"], "/scoped");
        assert_eq!(person_processing(&event), Some(&json!(false)));
    }

    #[test]
    fn an_oversized_event_keeps_its_interface() {
        let context = context(None);
        let names: Vec<String> = (0..3_000).map(|index| format!("{index:058}")).collect();
        let mut event = OutgoingEvent::new(TOOLS_LIST, &context);
        event.set(LISTED_TOOL_NAMES, names);
        let event = event.bounded(&[LISTED_TOOL_NAMES]);
        assert_eq!(event.properties[key::INTERFACE], INTERFACE);
        assert_eq!(event.properties[key::LIB], LIB_NAME);
        assert_eq!(event.properties[key::LIB_VERSION], CRATE_VERSION);
    }
}

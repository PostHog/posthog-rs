use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use super::context::EventContext;
use crate::client::CRATE_VERSION;
use crate::constants::PROCESS_PERSON_PROFILE_OPT;
use crate::mcp::bounds::fit_event_size;
use crate::mcp::contract::properties as key;
use crate::mcp::contract::{INTERFACE, LIB_NAME};
use crate::Event;

const ALWAYS_REQUIRED: [&str; 8] = [
    key::INTERFACE,
    key::SERVER_BUILD,
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
    host_properties: Map<String, Value>,
    options: Map<String, Value>,
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
            host_properties: context.host_properties.clone(),
            options: context.options.clone(),
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
        for (name, value) in self.options {
            let _ = event.insert_option(name, value);
        }
        if self.personless {
            let _ = event.insert_option(PROCESS_PERSON_PROFILE_OPT, false);
        }
        let _ = event.set_timestamp(self.timestamp);
        event.request_context_applied = true;
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
    use crate::constants::PROCESS_PERSON_PROFILE_PROP;
    use crate::mcp::capture::{host_options, host_properties};
    use crate::mcp::contract::events::{TOOLS_LIST, TOOL_CALL};
    use crate::mcp::contract::limits::MAX_PAYLOAD_STRING_LENGTH;
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
            server_build: None,
            host_properties: Map::new(),
            options: Map::new(),
        }
    }

    fn person_processing(event: &Event) -> Option<&Value> {
        event.options().get(PROCESS_PERSON_PROFILE_OPT)
    }

    /// An event of a call whose options ask for person processing.
    fn converted(identity: Option<Identity>, process_person_profile: bool) -> Event {
        let mut context = context(identity);
        context.options.insert(
            PROCESS_PERSON_PROFILE_OPT.to_string(),
            json!(process_person_profile),
        );
        OutgoingEvent::new(TOOL_CALL, &context).into_posthog_event()
    }

    #[test]
    fn an_anonymous_event_stays_personless_whatever_its_options_say() {
        let event = converted(None, true);
        assert_eq!(person_processing(&event), Some(&json!(false)));
        assert_eq!(event.distinct_id(), "ses_1");
        assert!(!event.properties().contains_key(PROCESS_PERSON_PROFILE_PROP));
    }

    #[test]
    fn host_options_read_legacy_option_properties_and_options_win() {
        let legacy = || {
            RequestContext::new()
                .property(PROCESS_PERSON_PROFILE_PROP, false)
                .unwrap()
        };
        let cases = [
            (RequestContext::new(), None),
            (legacy(), Some(json!(false))),
            (
                legacy().option(PROCESS_PERSON_PROFILE_OPT, true).unwrap(),
                Some(json!(true)),
            ),
        ];
        for (host, expected) in cases {
            let options = host_options(&host);
            assert_eq!(options.get(PROCESS_PERSON_PROFILE_OPT), expected.as_ref());
        }
    }

    #[test]
    fn an_identified_event_keeps_its_identity_and_takes_its_options() {
        let event = converted(Some(Identity::new("user-1")), false);
        assert_eq!(event.distinct_id(), "user-1");
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

    #[test]
    fn host_properties_are_bounded_with_the_event() {
        let mut context = context(None);
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
        let mut context = context(None);
        context.identity = Some(Identity::new("user-1").with_property("plan", "pro"));
        let host = json!({
            "authorization": "Bearer secret",
            "nested": { "password": "hunter2", "region": "eu" },
            "diagnostic": r#"upstream returned {"password":"hunter2"}"#,
            "last_error": "github said 401 for ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "$request_path": "/v1/charges/sk_live_abcdefghijklmnop1234",
            "tenant": "Acme_Corp_Production_Workspace",
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
            json!({ "password": "[redacted]", "region": "eu" })
        );
        assert_eq!(
            properties["diagnostic"],
            r#"upstream returned {"password":"[redacted]"}"#
        );
        assert_eq!(properties["last_error"], "github said 401 for [redacted]");
        assert_eq!(properties["$request_path"], "[redacted]");
        assert_eq!(properties["tenant"], "Acme_Corp_Production_Workspace");
        assert!(!properties.contains_key(PROCESS_PERSON_PROFILE_PROP));
        assert_eq!(
            host_options(&host).get(PROCESS_PERSON_PROFILE_OPT),
            Some(&json!(false))
        );
        assert_eq!(properties[key::SET], json!({ "plan": "pro" }));
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
        let mut context = context(None);
        let host = (0..4_000).fold(RequestContext::new(), |host, index| {
            host.property(format!("k{index:04}"), "v".repeat(40))
                .unwrap()
        });
        context.host_properties = host_properties(&host);
        let mut event = OutgoingEvent::new(TOOLS_LIST, &context);
        event.set(key::PARAMETERS, json!({ "q": "hello" }));
        event.set(key::RESPONSE, json!({ "a": "world" }));

        let event = event.bounded(&[]);

        assert_eq!(event.properties[key::PARAMETERS], json!({ "q": "hello" }));
        assert_eq!(event.properties[key::RESPONSE], json!({ "a": "world" }));
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

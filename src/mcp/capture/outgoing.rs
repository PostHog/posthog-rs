use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use super::context::EventContext;
use crate::client::CRATE_VERSION;
use crate::constants::{LEGACY_OPTION_PROPERTIES, PROCESS_PERSON_PROFILE_OPT};
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
    options: Map<String, Value>,
    request_context: Option<RequestContext>,
    /// The properties [`Self::bounded`] kept, so the event can be fitted again
    /// once the request context adds its own.
    kept: Option<Vec<&'static str>>,
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
            options: context.options.clone(),
            request_context: context.request_context.clone(),
            kept: None,
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
    pub(crate) fn bounded(mut self, required: &[&'static str]) -> Self {
        let mut kept: Vec<&'static str> = ALWAYS_REQUIRED.to_vec();
        kept.extend_from_slice(required);
        // A legacy option property, such as a `$process_person_profile: false`
        // from the request context, becomes an option only when the event is
        // sent, so dropping it to fit would lose the caller's choice.
        kept.extend(legacy_option_properties());
        fit_event_size(&mut self.properties, &kept);
        self.kept = Some(kept);
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
        // Applied before the MCP `before_send` rather than at enqueue, so what the hook returns is sent.
        if let Some(context) = RequestContext::current().or(self.request_context) {
            context.apply_to_event(&mut event);
            if let Some(kept) = &self.kept {
                fit_to_bound(&mut event, kept);
            }
        }
        event
    }
}

/// The legacy properties that set an event option.
fn legacy_option_properties() -> impl Iterator<Item = &'static str> {
    LEGACY_OPTION_PROPERTIES.iter().map(|&(prop, _)| prop)
}

/// Fits an event whose request context added properties after
/// [`OutgoingEvent::bounded`], keeping the same required properties.
fn fit_to_bound(event: &mut Event, kept: &[&str]) {
    let mut properties: Map<String, Value> = event
        .properties()
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    fit_event_size(&mut properties, kept);
    let removed: Vec<String> = event
        .properties()
        .keys()
        .filter(|name| !properties.contains_key(*name))
        .cloned()
        .collect();
    for name in removed {
        event.remove_prop(&name);
    }
    for (name, value) in properties {
        if event.properties().get(&name) != Some(&value) {
            let _ = event.insert_prop(name, value);
        }
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
    use crate::mcp::contract::limits::MAX_EVENT_BYTES;
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
            options: Map::new(),
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
    fn a_large_request_context_property_stays_within_the_bound() {
        let mut context = context(None);
        context.request_context = Some(
            RequestContext::new()
                .property("request_note", "x".repeat(200 * 1024))
                .unwrap(),
        );
        let event = OutgoingEvent::new(TOOL_CALL, &context)
            .bounded(&[])
            .into_posthog_event();
        let bytes = serde_json::to_vec(event.properties()).unwrap().len();
        assert!(bytes <= MAX_EVENT_BYTES, "{} bytes", bytes);
        assert_eq!(event.properties()[key::INTERFACE], INTERFACE);
        assert_eq!(event.properties()[key::LIB], LIB_NAME);
    }

    #[test]
    fn fitting_drops_an_optional_legacy_opt_out_unless_it_is_kept() {
        // Over the bound with a required list and a legacy opt-out as the only
        // optional property: unkept, the opt-out is dropped first; kept, the
        // list is trimmed instead.
        let over_bound = || {
            let names: Vec<String> = (0..3_000).map(|index| format!("{index:058}")).collect();
            let mut event = Event::new("$mcp_tools_list", "user-1");
            event.insert_prop(LISTED_TOOL_NAMES, names).unwrap();
            event.insert_prop("$process_person_profile", false).unwrap();
            event
        };
        let mut required: Vec<&'static str> = ALWAYS_REQUIRED.to_vec();
        required.push(LISTED_TOOL_NAMES);

        let mut unkept = over_bound();
        fit_to_bound(&mut unkept, &required);
        assert!(!unkept.properties().contains_key("$process_person_profile"));

        let mut kept = over_bound();
        required.extend(legacy_option_properties());
        fit_to_bound(&mut kept, &required);
        assert_eq!(
            kept.properties().get("$process_person_profile"),
            Some(&json!(false))
        );
        let bytes = serde_json::to_vec(kept.properties()).unwrap().len();
        assert!(bytes <= MAX_EVENT_BYTES, "{} bytes", bytes);
    }

    #[test]
    fn bounded_treats_legacy_option_properties_as_required() {
        let event = OutgoingEvent::new(TOOL_CALL, &context(None)).bounded(&[]);
        let kept = event.kept.as_ref().unwrap();
        for prop in legacy_option_properties() {
            assert!(kept.contains(&prop), "{} not kept", prop);
        }
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

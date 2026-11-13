//! Identify-style events (`$identify`, `$create_alias`, `$merge_dangerously`,
//! `$groupidentify`, `$set`, `$set_once`) send `process_person_profile: true`
//! unless the call sets the option or the legacy `$process_person_profile`
//! property, so no request context value can turn person processing off for
//! them. An event whose distinct ID the SDK generated keeps the personless
//! default.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use common::{record_wire_events, wire_event, WireEvents};
use httpmock::MockServer;
use posthog::{ClientOptionsBuilder, Event, RequestContext};
use serde_json::{json, Value};

const ANALYTICS_PATH: &str = "/i/v1/analytics/events";
const AI_PATH: &str = "/i/v1/ai/events";
const OPTION: &str = "process_person_profile";
const LEGACY: &str = "$process_person_profile";
const IDENTIFY_STYLE: [&str; 6] = [
    "$identify",
    "$create_alias",
    "$merge_dangerously",
    "$groupidentify",
    "$set",
    "$set_once",
];

// The same test bodies run on the async and the blocking client.
#[cfg(feature = "async-client")]
macro_rules! call {
    ($expr:expr) => {
        $expr.await
    };
}
#[cfg(not(feature = "async-client"))]
macro_rules! call {
    ($expr:expr) => {
        $expr
    };
}
macro_rules! client_test {
    ($name:ident, $body:block) => {
        #[cfg(feature = "async-client")]
        #[tokio::test]
        async fn $name() $body

        #[cfg(not(feature = "async-client"))]
        #[test]
        fn $name() $body
    };
}

/// One `process_person_profile` setup: what the call and the request context
/// set (`None` leaves a value unset), and the option the wire must carry
/// (`None` means absent).
struct Case {
    name: &'static str,
    generated_id: bool,
    option: Option<Value>,
    legacy: Option<Value>,
    context_option: Option<Value>,
    context_legacy: Option<Value>,
    expected: Option<Value>,
}

fn case(name: &'static str, expected: Option<Value>) -> Case {
    Case {
        name,
        generated_id: false,
        option: None,
        legacy: None,
        context_option: None,
        context_legacy: None,
        expected,
    }
}

impl Case {
    fn generated(mut self) -> Self {
        self.generated_id = true;
        self
    }

    fn option(mut self, value: Value) -> Self {
        self.option = Some(value);
        self
    }

    fn legacy(mut self, value: Value) -> Self {
        self.legacy = Some(value);
        self
    }

    fn context_option(mut self, value: Value) -> Self {
        self.context_option = Some(value);
        self
    }

    fn context_legacy(mut self, value: Value) -> Self {
        self.context_legacy = Some(value);
        self
    }

    fn tag(&self, event: &str) -> String {
        format!("{event}/{}", self.name)
    }

    /// The case's event named `event`, with a `case` property to find it on the wire.
    fn event(&self, event: &str) -> Event {
        let mut built = if self.generated_id {
            Event::new_anon(event)
        } else {
            Event::new(event, "user-1")
        };
        built.insert_prop("case", self.tag(event)).unwrap();
        if let Some(value) = &self.option {
            built.insert_option(OPTION, value).unwrap();
        }
        if let Some(value) = &self.legacy {
            built.insert_prop(LEGACY, value).unwrap();
        }
        built
    }

    fn context(&self) -> RequestContext {
        let mut context = RequestContext::new();
        if let Some(value) = &self.context_option {
            context = context.option(OPTION, value).unwrap();
        }
        if let Some(value) = &self.context_legacy {
            context = context.property(LEGACY, value).unwrap();
        }
        context
    }
}

/// Every boundary of the identify-style default, for each identify-style name.
fn identify_style_cases() -> Vec<Case> {
    let (t, f) = (|| json!(true), || json!(false));
    vec![
        case("unset", Some(t())),
        case("unset_context_option_false", Some(t())).context_option(f()),
        case("unset_context_legacy_false", Some(t())).context_legacy(f()),
        case("unset_context_both_false", Some(t()))
            .context_option(f())
            .context_legacy(f()),
        case("option_false_beats_context_true", Some(f()))
            .option(f())
            .context_option(t())
            .context_legacy(t()),
        case("option_true_beats_context_false", Some(t()))
            .option(t())
            .context_option(f())
            .context_legacy(f()),
        case("legacy_false", Some(f())).legacy(f()),
        case("legacy_false_beats_context_true", Some(f()))
            .legacy(f())
            .context_option(t()),
        case("legacy_true_beats_context_false", Some(t()))
            .legacy(t())
            .context_option(f())
            .context_legacy(f()),
        case("legacy_sent_as_given", Some(json!("false")))
            .legacy(json!("false"))
            .context_option(t()),
        case("option_false_beats_legacy_true", Some(f()))
            .option(f())
            .legacy(t()),
        case("option_true_beats_legacy_false", Some(t()))
            .option(t())
            .legacy(f()),
        case("null_option_is_unset", Some(t()))
            .option(Value::Null)
            .context_option(f()),
        case("null_legacy_is_unset", Some(t()))
            .legacy(Value::Null)
            .context_option(f()),
        case("null_both_is_unset", Some(t()))
            .option(Value::Null)
            .legacy(Value::Null)
            .context_legacy(f()),
        // A generated distinct ID gets no default, so the ordinary layers apply.
        case("generated", Some(f())).generated(),
        case("generated_context_option_false", Some(f()))
            .generated()
            .context_option(f()),
        case("generated_context_option_true", Some(t()))
            .generated()
            .context_option(t()),
        case("generated_context_legacy_true", Some(f()))
            .generated()
            .context_legacy(t()),
        case("generated_option_true", Some(t()))
            .generated()
            .option(t()),
        case("generated_legacy_true", Some(t()))
            .generated()
            .legacy(t()),
        case("generated_legacy_true_beats_context_false", Some(t()))
            .generated()
            .legacy(t())
            .context_option(f()),
    ]
}

/// Other events keep the ordinary layering, including a context option over a
/// legacy property.
fn other_event_cases() -> Vec<Case> {
    let (t, f) = (|| json!(true), || json!(false));
    vec![
        case("unset", None),
        case("context_option_false", Some(f())).context_option(f()),
        case("context_legacy_false", Some(f())).context_legacy(f()),
        case("legacy_false_loses_to_context_true", Some(t()))
            .legacy(f())
            .context_option(t()),
        case("generated", Some(f())).generated(),
    ]
}

/// Each case's event for every name it applies to, with its expected wire option.
fn case_events() -> Vec<(Case, &'static str)> {
    let mut events: Vec<(Case, &'static str)> = Vec::new();
    for name in IDENTIFY_STYLE {
        events.extend(identify_style_cases().into_iter().map(|case| (case, name)));
    }
    events.extend(other_event_cases().into_iter().map(|case| (case, "work")));
    events
}

/// The legacy property never reaches the wire: capture moves it into the option.
fn assert_option(event: &Value, expected: Option<&Value>, label: &str) {
    assert_eq!(event["options"].get(OPTION), expected, "option, {label}");
    assert!(
        event["properties"].get(LEGACY).is_none(),
        "legacy property, {}",
        label
    );
}

fn assert_wire_option(events: &WireEvents, tag: &str, expected: Option<&Value>) {
    assert_option(&wire_event(events, Some(tag)), expected, tag);
}

fn assert_cases(events: &WireEvents, cases: &[(Case, &str)]) {
    for (case, name) in cases {
        assert_wire_option(events, &case.tag(name), case.expected.as_ref());
    }
}

fn options(server: &MockServer) -> ClientOptionsBuilder {
    let mut builder = ClientOptionsBuilder::default();
    builder
        .api_key("phc_test".to_string())
        .host(server.base_url())
        .max_capture_attempts(1u32);
    builder
}

client_test!(pre_applied_context_never_turns_off_identify_style_events, {
    let server = MockServer::start();
    let events = record_wire_events(&server, ANALYTICS_PATH);
    let client = call!(posthog::client(options(&server).build().unwrap()));
    let cases = case_events();
    for (case, name) in &cases {
        let mut event = case.event(name);
        case.context().apply_to_event(&mut event);
        client.capture(event);
    }
    call!(client.flush());
    assert_cases(&events, &cases);
});

client_test!(every_capture_method_applies_the_identify_style_default, {
    let server = MockServer::start();
    let events = record_wire_events(&server, ANALYTICS_PATH);
    let ai_events = record_wire_events(&server, AI_PATH);
    let client = call!(posthog::client(options(&server).build().unwrap()));
    // `$identify` gets the default with no request context; `work` does not.
    let pair = |method: &str| {
        let event = |name: &str| {
            let mut event = Event::new(name, "user-1");
            event
                .insert_prop("case", format!("{method}/{name}"))
                .unwrap();
            event
        };
        vec![event("$identify"), event("work")]
    };
    client.capture_batch(pair("capture_batch"), false);
    client.capture_batch(pair("capture_batch_historical"), true);
    client.capture_ai_batch(pair("capture_ai_batch"), false);
    for event in pair("capture") {
        client.capture(event);
    }
    for event in pair("capture_ai") {
        client.capture_ai(event);
    }
    for event in pair("capture_immediate") {
        call!(client.capture_immediate(event)).unwrap();
    }
    for event in pair("capture_ai_immediate") {
        call!(client.capture_ai_immediate(event)).unwrap();
    }
    call!(client.capture_batch_immediate(pair("capture_batch_immediate"), false)).unwrap();
    call!(client.capture_ai_batch_immediate(pair("capture_ai_batch_immediate"), false)).unwrap();
    call!(client.flush());

    for (recorded, methods) in [
        (
            &events,
            [
                "capture",
                "capture_batch",
                "capture_batch_historical",
                "capture_immediate",
                "capture_batch_immediate",
            ]
            .as_slice(),
        ),
        (
            &ai_events,
            [
                "capture_ai",
                "capture_ai_batch",
                "capture_ai_immediate",
                "capture_ai_batch_immediate",
            ]
            .as_slice(),
        ),
    ] {
        for method in methods {
            assert_wire_option(recorded, &format!("{method}/$identify"), Some(&json!(true)));
            assert_wire_option(recorded, &format!("{method}/work"), None);
        }
    }
});

client_test!(context_built_events_resolve_before_the_context_fills, {
    let server = MockServer::start();
    let events = record_wire_events(&server, ANALYTICS_PATH);
    let client = call!(posthog::client(options(&server).build().unwrap()));
    let tagged = |mut event: Event, tag: &str| {
        event.insert_prop("case", tag).unwrap();
        event
    };
    let identified = RequestContext::new()
        .distinct_id("context-user")
        .option(OPTION, false)
        .unwrap();
    let anonymous_false = RequestContext::new().option(OPTION, false).unwrap();
    let anonymous_legacy_true = RequestContext::new().property(LEGACY, true).unwrap();
    client.capture(tagged(identified.event("$identify"), "identified"));
    client.capture(tagged(anonymous_false.event("$identify"), "anonymous"));
    // The context's legacy property is not the caller's, at capture or on a second apply.
    let mut applied_twice = anonymous_legacy_true.event("$identify");
    anonymous_legacy_true.apply_to_event(&mut applied_twice);
    client.capture(tagged(
        anonymous_legacy_true.event("$identify"),
        "anonymous_legacy",
    ));
    client.capture(tagged(applied_twice, "applied_twice"));
    call!(client.flush());

    assert_wire_option(&events, "identified", Some(&json!(true)));
    assert_wire_option(&events, "anonymous", Some(&json!(false)));
    assert_wire_option(&events, "anonymous_legacy", Some(&json!(false)));
    assert_wire_option(&events, "applied_twice", Some(&json!(false)));
});

client_test!(event_alias_and_group_identify_take_a_call_option, {
    let server = MockServer::start();
    let events = record_wire_events(&server, ANALYTICS_PATH);
    let client = call!(posthog::client(options(&server).build().unwrap()));
    let alias = || Event::alias("anon-1", "user-1").unwrap();
    let group = || Event::group_identify("company", "acme", json!({"name": "Acme"})).unwrap();
    let context_false = RequestContext::new().option(OPTION, false).unwrap();
    for (tag, mut event) in [("alias", alias()), ("group_identify", group())] {
        let mut defaulted = event.clone();
        defaulted
            .insert_prop("case", format!("{tag}/default"))
            .unwrap();
        context_false.apply_to_event(&mut defaulted);
        client.capture(defaulted);

        event.insert_prop("case", format!("{tag}/option")).unwrap();
        event.insert_option(OPTION, false).unwrap();
        context_false.apply_to_event(&mut event);
        client.capture(event);
    }
    call!(client.flush());

    for tag in ["alias", "group_identify"] {
        assert_wire_option(&events, &format!("{tag}/default"), Some(&json!(true)));
        assert_wire_option(&events, &format!("{tag}/option"), Some(&json!(false)));
    }
});

client_test!(before_send_sees_the_default_and_has_the_final_say, {
    let server = MockServer::start();
    let events = record_wire_events(&server, ANALYTICS_PATH);
    let seen = Arc::new(Mutex::new(None));
    let hook_seen = seen.clone();
    let mut builder = options(&server);
    builder.before_send(move |mut event| {
        match event.event_name() {
            "$identify" => {
                *hook_seen.lock().unwrap() = event.options().get(OPTION).cloned();
                event.insert_option(OPTION, false).unwrap();
                Some(event)
            }
            // A hook that renames an event into an identify-style one gets no default.
            "work" => {
                let mut renamed = Event::new("$identify", event.distinct_id());
                for (key, value) in event.properties() {
                    renamed.insert_prop(key.as_str(), value).unwrap();
                }
                Some(renamed)
            }
            _ => Some(event),
        }
    });
    let client = call!(posthog::client(builder.build().unwrap()));
    for (name, tag) in [("$identify", "flipped"), ("work", "renamed")] {
        let mut event = Event::new(name, "user-1");
        event.insert_prop("case", tag).unwrap();
        client.capture(event);
    }
    call!(client.flush());

    assert_eq!(*seen.lock().unwrap(), Some(json!(true)));
    assert_wire_option(&events, "flipped", Some(&json!(false)));
    let renamed = wire_event(&events, Some("renamed"));
    assert_eq!(renamed["event"], "$identify");
    assert_wire_option(&events, "renamed", None);
});

/// The `process_person_profile` option and legacy property a `before_send` hook saw.
type HookView = (Option<Value>, Option<Value>);

client_test!(before_send_never_sees_a_shadowed_legacy_property, {
    let server = MockServer::start();
    let events = record_wire_events(&server, ANALYTICS_PATH);
    let seen: Arc<Mutex<HashMap<String, HookView>>> = Arc::default();
    let hook_seen = seen.clone();
    let mut builder = options(&server);
    builder.before_send(move |mut event| {
        let tag = event.properties()["case"].as_str().unwrap().to_string();
        let view = (
            event.options().get(OPTION).cloned(),
            event.properties().get(LEGACY).cloned(),
        );
        if tag.ends_with("option_removed") {
            event.remove_option(OPTION);
        }
        hook_seen.lock().unwrap().insert(tag, view);
        Some(event)
    });
    let client = call!(posthog::client(builder.build().unwrap()));
    let (t, f) = (|| json!(true), || json!(false));
    // A hook that removes the option sends none when the property was shadowed.
    let cases: Vec<(Case, &str, HookView)> = vec![
        (
            case("context_legacy", Some(t())).context_legacy(f()),
            "$identify",
            (Some(t()), None),
        ),
        (
            case("context_legacy_option_removed", None).context_legacy(f()),
            "$identify",
            (Some(t()), None),
        ),
        (
            case("generated_option_removed", None)
                .generated()
                .context_legacy(t()),
            "$identify",
            (Some(f()), None),
        ),
        (
            case("option_and_legacy", Some(f())).option(f()).legacy(t()),
            "$identify",
            (Some(f()), None),
        ),
        // Other events keep the property, so the hook sees it as before.
        (
            case("context_legacy", Some(f())).context_legacy(f()),
            "work",
            (None, Some(f())),
        ),
        (
            case("context_legacy_option_removed", Some(f())).context_legacy(f()),
            "work",
            (None, Some(f())),
        ),
    ];
    for (case, name, _) in &cases {
        let mut event = case.event(name);
        case.context().apply_to_event(&mut event);
        client.capture(event);
    }
    call!(client.flush());

    let seen = seen.lock().unwrap();
    for (case, name, view) in &cases {
        let tag = case.tag(name);
        assert_eq!(seen.get(&tag), Some(view), "hook, {tag}");
        assert_wire_option(&events, &tag, case.expected.as_ref());
    }
});

#[cfg(feature = "async-client")]
mod scoped_context {
    use super::*;

    #[tokio::test]
    async fn scoped_context_never_turns_off_identify_style_events() {
        let server = MockServer::start();
        let events = record_wire_events(&server, ANALYTICS_PATH);
        let client = posthog::client(options(&server).build().unwrap()).await;
        let cases = case_events();
        for (case, name) in &cases {
            case.context()
                .scope(async { client.capture(case.event(name)) })
                .await;
        }
        client.flush().await;
        assert_cases(&events, &cases);
    }

    #[tokio::test]
    async fn every_emitter_overrides_a_scoped_false() {
        let server = MockServer::start();
        let events = record_wire_events(&server, ANALYTICS_PATH);
        let ai_events = record_wire_events(&server, AI_PATH);
        let client = posthog::client(options(&server).build().unwrap()).await;
        let tagged = |name: &str, tag: &str| {
            let mut event = Event::new(name, "user-1");
            event.insert_prop("case", tag).unwrap();
            event
        };
        for identity in [Some("context-user"), None] {
            let mut context = RequestContext::new()
                .option(OPTION, false)
                .unwrap()
                .property(LEGACY, false)
                .unwrap();
            if let Some(identity) = identity {
                context = context.distinct_id(identity);
            }
            let suffix = if identity.is_some() { "id" } else { "no_id" };
            context
                .scope(async {
                    client.alias(format!("anon-{suffix}"), "user-1");
                    client
                        .group_identify("company", format!("acme-{suffix}"), json!({}))
                        .unwrap();
                    client.capture(tagged("$identify", &format!("capture/{suffix}")));
                    client.capture_batch(
                        vec![tagged("$set", &format!("capture_batch/{suffix}"))],
                        false,
                    );
                    client
                        .capture_immediate(tagged("$set_once", &format!("immediate/{suffix}")))
                        .await
                        .unwrap();
                    client.capture_ai(tagged("$identify", &format!("capture_ai/{suffix}")));
                    client
                        .capture_ai_immediate(tagged(
                            "$identify",
                            &format!("capture_ai_immediate/{suffix}"),
                        ))
                        .await
                        .unwrap();
                    client.capture(tagged("work", &format!("work/{suffix}")));
                })
                .await;
        }
        client.flush().await;

        let recorded = events.lock().unwrap().clone();
        for suffix in ["id", "no_id"] {
            let alias = recorded
                .values()
                .find(|event| {
                    event["properties"]["alias"] == "user-1"
                        && event["distinct_id"] == format!("anon-{suffix}")
                })
                .unwrap_or_else(|| panic!("alias {}", suffix));
            assert_option(alias, Some(&json!(true)), &format!("alias {suffix}"));
            let group = recorded
                .values()
                .find(|event| event["properties"]["$group_key"] == format!("acme-{suffix}"))
                .unwrap_or_else(|| panic!("group_identify {}", suffix));
            assert_option(
                group,
                Some(&json!(true)),
                &format!("group_identify {suffix}"),
            );
            for method in ["capture", "capture_batch", "immediate"] {
                assert_wire_option(&events, &format!("{method}/{suffix}"), Some(&json!(true)));
            }
            for method in ["capture_ai", "capture_ai_immediate"] {
                assert_wire_option(
                    &ai_events,
                    &format!("{method}/{suffix}"),
                    Some(&json!(true)),
                );
            }
            assert_wire_option(&events, &format!("work/{suffix}"), Some(&json!(false)));
        }
    }
}

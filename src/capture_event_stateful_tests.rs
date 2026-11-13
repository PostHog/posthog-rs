use super::*;
use crate::{BeforeSendHook, Event};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn now() -> DateTime<Utc> {
    "2025-01-01T00:00:00.123456789Z".parse().unwrap()
}

#[test]
fn stateful_conversion_retains_sdk_properties_and_shares_server_wire_rules() {
    let mut event = Event::new("captured", "identity-at-capture");
    event.set_uuid(Uuid::now_v7());
    event
        .set_timestamp(DateTime::parse_from_rfc3339("2024-12-31T21:00:00.123456789-03:00").unwrap())
        .unwrap();
    for (key, value) in [
        ("$lib", json!("embedded-sdk")),
        ("$lib_version", json!("2.3.4")),
        (
            "$groups",
            json!({"company": "property-value", "project": "kept"}),
        ),
        ("$session_id", json!("session-at-capture")),
        ("$window_id", json!("window-at-capture")),
        ("$process_person_profile", json!(false)),
        ("$cookieless_mode", json!(true)),
        ("$set", json!({"plan": "pro"})),
        ("custom", json!({"items": [false, 0, "", {}]})),
    ] {
        event.insert_prop(key, value).unwrap();
    }
    event.add_group("company", "typed-group");
    event.insert_option("process_person_profile", true).unwrap();
    event.insert_option("future_option", Value::Null).unwrap();
    let original = event.clone();
    let wire = CaptureEvent::from_stateful_event_at(&event, now());
    assert_eq!(wire.properties["$lib"], "embedded-sdk");
    assert_eq!(wire.properties["$lib_version"], "2.3.4");
    assert_eq!(
        wire.properties["$groups"],
        json!({"company": "typed-group", "project": "kept"})
    );
    assert_eq!(wire.options["process_person_profile"], true);
    assert_eq!(wire.options["cookieless_mode"], true);
    assert_eq!(wire.options["future_option"], Value::Null);
    assert_eq!(wire.session_id.as_deref(), Some("session-at-capture"));
    assert_eq!(wire.window_id.as_deref(), Some("window-at-capture"));
    assert_eq!(wire.timestamp, "2025-01-01T00:00:00.123456789Z");
    assert_eq!(wire.distinct_id, "identity-at-capture");
    assert_eq!(Some(wire.uuid), event.uuid());
    assert_eq!(
        serde_json::to_value(wire).unwrap(),
        serde_json::to_value(CaptureEvent::from_event_at(&event, now())).unwrap()
    );
    assert_eq!(
        event, original,
        "conversion must not mutate the producer snapshot"
    );
}

#[test]
fn stateful_conversion_does_not_invent_attribution_or_replace_capture_time() {
    let mut event = Event::new("without-sdk-properties", "anonymous-id");
    event.set_uuid(Uuid::now_v7());
    let wire = CaptureEvent::from_stateful_event_at(&event, now());
    assert_eq!(wire.properties, json!({}));
    assert_eq!(wire.timestamp, "2025-01-01T00:00:00.123456789Z");
    assert_eq!(Some(wire.uuid), event.uuid());
    assert_eq!(wire.distinct_id, "anonymous-id");
}

#[test]
fn stateful_conversion_preserves_native_hook_attribution_changes_and_removals() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let hook = BeforeSendHook::new(move |mut event| {
        observed.fetch_add(1, Ordering::SeqCst);
        event.insert_prop("$lib", "hook-sdk").unwrap();
        event.remove_prop("$lib_version");
        Some(event)
    });
    let mut event = Event::new("transformed", "anonymous-id");
    event.insert_prop("$lib", "original-sdk").unwrap();
    event
        .insert_prop("$lib_version", "original-version")
        .unwrap();
    let transformed = hook.apply(event).unwrap();
    let wire = CaptureEvent::from_stateful_event_at(&transformed, now());
    assert_eq!(wire.properties, json!({"$lib": "hook-sdk"}));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

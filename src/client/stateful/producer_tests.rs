use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn now() -> DateTime<Utc> {
    "2025-01-01T00:00:00.123456789Z".parse().unwrap()
}

fn snapshot() -> Snapshot {
    Snapshot::anonymous("anonymous-id".into())
}

#[test]
fn assembly_preserves_rust_precedence_and_captures_identity_time_and_sdk() {
    let mut config = Config::new("phc_test");
    config.sdk_name = "embedding".into();
    config.sdk_version = "1.2.3".into();
    config.disable_geoip = true;
    config
        .platform_context
        .insert("$os".into(), "platform-os".into());
    config
        .platform_context
        .insert("$set".into(), json!({"plan": "context", "only_context": 1}));
    config
        .platform_context
        .insert("context_only".into(), true.into());
    let producer = Producer::new(&config);
    let mut input = Capture::new("captured");
    input.uuid = Some(Uuid::now_v7());
    input.properties.insert("$os".into(), "caller-os".into());
    input
        .properties
        .insert("$set".into(), json!({"plan": "caller"}));
    input.properties.insert(
        "$groups".into(),
        json!({"company": "property", "project": "kept"}),
    );
    input
        .properties
        .insert("$session_id".into(), "session".into());
    input
        .properties
        .insert("$window_id".into(), "window".into());
    input
        .properties
        .insert("$process_person_profile".into(), false.into());
    input
        .options
        .insert("process_person_profile".into(), true.into());
    input.groups.insert("company".into(), "typed".into());
    let wire = producer.prepare(input.clone(), &snapshot(), now()).unwrap();
    assert_eq!(wire.event, "captured");
    assert_eq!(wire.distinct_id, "anonymous-id");
    assert_eq!(Some(wire.uuid), input.uuid);
    assert_eq!(wire.timestamp, "2025-01-01T00:00:00.123456789Z");
    assert_eq!(wire.session_id.as_deref(), Some("session"));
    assert_eq!(wire.window_id.as_deref(), Some("window"));
    assert_eq!(wire.properties["$os"], "caller-os");
    assert_eq!(
        wire.properties["$set"],
        json!({"plan": "caller", "only_context": 1})
    );
    assert_eq!(
        wire.properties["$groups"],
        json!({"company": "typed", "project": "kept"})
    );
    assert_eq!(wire.properties["context_only"], true);
    assert_eq!(wire.properties["$device_id"], "anonymous-id");
    assert_eq!(wire.properties["$is_identified"], false);
    assert_eq!(wire.properties["$geoip_disable"], true);
    assert_eq!(wire.properties["$lib"], "embedding");
    assert_eq!(wire.properties["$lib_version"], "1.2.3");
    assert_eq!(wire.options["process_person_profile"], true);
    assert!(wire.properties.get("$is_server").is_none());
}

#[test]
fn normalization_omits_custom_null_members_but_preserves_slots_and_protocol_payloads() {
    let config = Config::new("phc_test");
    let producer = Producer::new(&config);
    let mut input = Capture::new("nullable");
    input.properties = serde_json::from_value(json!({
        "drop": null, "nested": {"drop": null, "keep": 1},
        "items": [null, {"drop": null}, [null], false, 0, "", "null", "undefined", {}, []],
        "$set": {"clear": null}, "$set_once": {"clear": null},
        "$group_set": {"clear": null}, "$groups": {"company": null},
        "$session_id": null, "$process_person_profile": true
    }))
    .unwrap();
    input
        .options
        .insert("future_option".into(), json!({"null": null}));
    input
        .options
        .insert("process_person_profile".into(), Value::Null);
    let wire = producer.prepare(input, &snapshot(), now()).unwrap();
    assert!(wire.properties.get("drop").is_none());
    assert_eq!(wire.properties["nested"], json!({"keep": 1}));
    assert_eq!(
        wire.properties["items"],
        json!([null, {}, [null], false, 0, "", "null", "undefined", {}, []])
    );
    for key in ["$set", "$set_once", "$group_set"] {
        assert_eq!(wire.properties[key], json!({"clear": null}));
    }
    assert_eq!(wire.properties["$groups"], json!({"company": null}));
    assert_eq!(wire.options["future_option"], json!({"null": null}));
    assert_eq!(wire.options["process_person_profile"], false);
    assert!(wire.session_id.is_none());
    assert!(wire.properties.get("missing").is_none());
}

#[test]
fn hooks_see_enrichment_in_order_and_cleanup_does_not_restore_removed_data() {
    let mut config = Config::new("phc_test");
    config.before_send(|mut event| {
        assert_eq!(event.properties()["$device_id"], "anonymous-id");
        assert!(event.properties().contains_key("$lib"));
        assert!(event.uuid().is_some());
        assert_eq!(event.timestamp(), Some(now().naive_utc()));
        event.insert_prop("step", 1).unwrap();
        event.remove_prop("$lib_version");
        event.remove_option("process_person_profile");
        Some(event)
    });
    config.before_send(|mut event| {
        assert_eq!(event.properties()["step"], 1);
        event.insert_prop("hook_null", Value::Null).unwrap();
        event
            .insert_prop("hook_array", json!([null, {"drop": null}]))
            .unwrap();
        event.insert_prop("$lib", "hook-sdk").unwrap();
        Some(event)
    });
    let wire = Producer::new(&config)
        .prepare(Capture::new("hooked"), &snapshot(), now())
        .unwrap();
    assert_eq!(wire.properties["$lib"], "hook-sdk");
    assert!(wire.properties.get("$lib_version").is_none());
    assert!(wire.properties.get("hook_null").is_none());
    assert_eq!(wire.properties["hook_array"], json!([null, {}]));
    assert!(!wire.options.contains_key("process_person_profile"));
}

#[test]
fn drop_and_panic_stop_the_chain_and_never_return_the_original() {
    for panic in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let later = calls.clone();
        let mut config = Config::new("phc_test");
        config.before_send(move |_| {
            if panic {
                panic!("test hook failure");
            }
            None
        });
        config.before_send(move |event| {
            later.fetch_add(1, Ordering::SeqCst);
            Some(event)
        });
        let producer = Producer::new(&config);
        // A panic poisons the hook mutex; subsequent captures must still fail closed.
        for _ in 0..2 {
            assert!(matches!(
                producer.prepare(Capture::new("private"), &snapshot(), now()),
                Err(CaptureOutcome::Dropped)
            ));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn input_and_transformed_names_are_validated_without_reenrichment() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut config = Config::new("phc_test");
    config.before_send(move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        Some(Event::new(" ", "replacement-id"))
    });
    let producer = Producer::new(&config);
    assert!(matches!(
        producer.prepare(Capture::new(" "), &snapshot(), now()),
        Err(CaptureOutcome::Invalid)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        producer.prepare(Capture::new("valid"), &snapshot(), now()),
        Err(CaptureOutcome::Invalid)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn replacement_hooks_keep_their_identity_and_metadata_removals() {
    let mut config = Config::new("phc_test");
    config.before_send(|_| Some(Event::new("replacement", "replacement-id")));
    let wire = Producer::new(&config)
        .prepare(Capture::new("original"), &snapshot(), now())
        .unwrap();
    assert_eq!(wire.event, "replacement");
    assert_eq!(wire.distinct_id, "replacement-id");
    assert_eq!(wire.properties, json!({}));
    assert!(wire.options.is_empty());
    assert_eq!(wire.timestamp, "2025-01-01T00:00:00.123456789Z");
}

#[test]
fn unspecified_uuid_is_fresh_per_capture_and_explicit_time_is_kept() {
    let producer = Producer::new(&Config::new("phc_test"));
    let mut input = Capture::new("repeat");
    input.timestamp = Some("2024-01-01T00:00:00Z".parse().unwrap());
    let first = producer.prepare(input.clone(), &snapshot(), now()).unwrap();
    let second = producer.prepare(input, &snapshot(), now()).unwrap();
    assert_ne!(first.uuid, second.uuid);
    assert_eq!(first.uuid.get_version_num(), 7);
    assert_eq!(first.timestamp, "2024-01-01T00:00:00.000Z");
    assert_eq!(first.timestamp, second.timestamp);
}

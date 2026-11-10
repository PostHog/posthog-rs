#![cfg(feature = "stateful")]

use httpmock::{Method::POST, MockServer};
use posthog::stateful::{Client, Config, DeliveryOutcome, Persistence, PersonProfiles};
use posthog::{Event, FlagValue};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc, Arc, Mutex, Weak,
};
use std::time::Duration;

fn config(server: &MockServer) -> Config {
    let mut c = Config::new("phc_test");
    c.host = server.base_url();
    c.persistence = Persistence::Memory;
    c.preload_feature_flags = false;
    c.bootstrap_anonymous_id = Some("device".into());
    c.bootstrap_feature_flags = Some(HashMap::from([
        ("a".into(), FlagValue::Boolean(true)),
        ("b".into(), FlagValue::String("blue".into())),
    ]));
    c.bootstrap_feature_flag_payloads = Some(HashMap::from([
        ("a".into(), Value::Null),
        ("b".into(), json!({"nested":null})),
    ]));
    c.capture_compression = None;
    c.flush_at = 1000;
    c.flush_interval = Duration::from_secs(3600);
    c.max_capture_attempts = 1;
    c.shutdown_timeout = Duration::from_secs(1);
    c
}

fn accept(server: &MockServer) {
    server.mock(|when, then| {
        when.path("/i/v1/analytics/events");
        then.status(200).json_body(json!({"results":{}}));
    });
}

fn observe(c: &mut Config) -> Arc<Mutex<Vec<Event>>> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    c.before_send(move |event| {
        seen.lock().unwrap().push(event.clone());
        Some(event)
    });
    events
}

#[test]
fn reports_are_not_restored_from_the_persistent_flag_cache() {
    let server = MockServer::start();
    accept(&server);
    let directory = std::env::temp_dir().join(format!("posthog-reports-{}", uuid::Uuid::now_v7()));
    for restored in [false, true] {
        let mut c = config(&server);
        c.persistence = Persistence::Directory(directory.clone());
        if restored {
            c.bootstrap_feature_flags = None;
            c.bootstrap_feature_flag_payloads = None;
        }
        let events = observe(&mut c);
        let client = Client::init_blocking(c);
        assert_eq!(
            client.get_feature_flag("a", None),
            Some(FlagValue::Boolean(true))
        );
        assert_eq!(
            client.get_feature_flag("a", None),
            Some(FlagValue::Boolean(true))
        );
        assert_eq!(events.lock().unwrap().len(), 1);
        assert_eq!(
            client.shutdown_blocking(None).state,
            posthog::stateful::PersistenceOutcome::Persisted
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(feature = "async-client")]
#[test]
fn runtime_free_async_client_reports_synchronously_from_cached_getters() {
    let server = MockServer::start();
    accept(&server);
    let mut c = config(&server);
    let events = observe(&mut c);
    futures::executor::block_on(async {
        let client = Client::init(c).await;
        assert_eq!(
            client.get_feature_flag("a", None),
            Some(FlagValue::Boolean(true))
        );
        assert_eq!(events.lock().unwrap().len(), 1);
        client.shutdown(None).await;
    });
}

#[test]
fn defaults_overrides_all_getters_and_consent_share_accepted_dedup() {
    for default in [false, true] {
        let server = MockServer::start();
        accept(&server);
        let mut c = config(&server);
        assert!(c.send_feature_flag_event);
        c.send_feature_flag_event = default;
        c.person_profiles = PersonProfiles::Never;
        let events = observe(&mut c);
        let client = Client::init_blocking(c);
        assert_eq!(
            client.get_feature_flag("a", Some(false)),
            Some(FlagValue::Boolean(true))
        );
        assert_eq!(client.get_feature_flag_result("missing", None), None);
        assert_eq!(client.is_feature_enabled("missing", None), None);
        assert!(events.lock().unwrap().is_empty());
        let result = client.get_feature_flag_result("a", None).unwrap();
        assert!(result.enabled);
        assert_eq!(result.variant, None);
        assert_eq!(result.payload, Some(Value::Null));
        assert_eq!(result.context.distinct_id, "device");
        assert_eq!(events.lock().unwrap().len(), usize::from(default));
        assert_eq!(client.is_feature_enabled("a", Some(true)), Some(true));
        assert_eq!(
            client.get_feature_flag("a", Some(true)),
            Some(FlagValue::Boolean(true))
        );
        assert_eq!(events.lock().unwrap().len(), 1);
        client.opt_out();
        assert_eq!(
            client.get_feature_flag("b", Some(true)),
            Some(FlagValue::String("blue".into()))
        );
        assert_eq!(events.lock().unwrap().len(), 1);
        client.opt_in();
        assert_eq!(
            client.get_feature_flag("b", Some(true)),
            Some(FlagValue::String("blue".into()))
        );
        assert_eq!(events.lock().unwrap().len(), 2);
        assert_eq!(client.peek_session_id(), None);
        client.shutdown_blocking(None);
        assert_eq!(client.get_feature_flag("a", None), None);
        assert_eq!(client.get_feature_flag_result("a", None), None);
    }
}

#[test]
fn minimization_is_pinned_and_final_after_hooks_with_wire_options_preserved() {
    for gate in [false, true] {
        for experiment in [None, Some(false), Some(true)] {
            let server = MockServer::start();
            let minimal = gate && experiment == Some(false);
            let mut metadata = json!({"id":12,"version":3,"payload":"{\"nested\":null}"});
            if let Some(experiment) = experiment {
                metadata["has_experiment"] = json!(experiment);
            }
            let flags = server.mock(|when, then| {
            when.path("/flags/");
            then.status(200).json_body(json!({"flags":{"flag":{"key":"flag","enabled":false,"variant":"ignored", "metadata":metadata}},
                "requestId":"request-a","evaluatedAt":123,"minimalFlagCalledEvents":gate}));
        });
            let id = uuid::Uuid::now_v7();
            let wire = server.mock(|when, then| {
                let when = when
                    .method(POST)
                    .path("/i/v1/analytics/events")
                    .body_includes("\"event\":\"$feature_flag_called\"")
                    .body_includes("\"$feature_flag_response\":false")
                    .body_includes("\"$feature_flag_request_id\":\"request-a\"")
                    .body_includes("\"$feature_flag_id\":12")
                    .body_includes("\"process_person_profile\":false")
                    .body_includes("\"session_id\":");
                if minimal {
                    when.body_excludes("private-super")
                        .body_excludes("hook-private")
                        .body_excludes("$feature_flag_payload");
                } else {
                    when.body_includes("private-super")
                        .body_includes("hook-private")
                        .body_includes("\"$feature_flag_payload\":{\"nested\":null}");
                }
                then.status(200)
                    .json_body(json!({"results":{(id.to_string()):{"result":"ok"}}}));
            });
            let mut c = config(&server);
            c.bootstrap_feature_flags = None;
            let hooks = Arc::new(AtomicUsize::new(0));
            let seen = hooks.clone();
            c.before_send(move |mut event| {
                assert_eq!(event.properties()["private-super"], true);
                seen.fetch_add(1, Ordering::SeqCst);
                event.insert_prop("hook-private", json!(true)).unwrap();
                event.set_uuid(id);
                Some(event)
            });
            let client = Client::init_blocking(c);
            client.register(HashMap::from([("private-super".into(), json!(true))]));
            client.start_session();
            let session = client.peek_session_id();
            client.reload_feature_flags();
            client.flush_blocking();
            let result = client.get_feature_flag_result("flag", None).unwrap();
            assert!(!result.enabled);
            assert_eq!(result.variant, None);
            assert_eq!(result.id, Some(12));
            assert_eq!(result.payload, Some(json!({"nested":null})));
            assert_eq!(
                client.get_feature_flag("flag", None),
                Some(FlagValue::Boolean(false))
            );
            assert_eq!(client.is_feature_enabled("flag", None), Some(false));
            assert_eq!(client.peek_session_id(), session);
            client.flush_blocking();
            flags.assert_calls(1);
            wire.assert_calls(1);
            assert_eq!(hooks.load(Ordering::SeqCst), 1);
        }
    }
}

#[test]
fn dropped_and_panicking_hooks_release_reservations() {
    let server = MockServer::start();
    accept(&server);
    let mut c = config(&server);
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    c.before_send(move |event| match seen.fetch_add(1, Ordering::SeqCst) {
        0 => panic!("test hook failure"),
        1 => None,
        _ => Some(event),
    });
    let client = Client::init_blocking(c);
    for _ in 0..4 {
        assert_eq!(
            client.get_feature_flag("a", None),
            Some(FlagValue::Boolean(true))
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn full_queue_does_not_commit_a_report() {
    let server = MockServer::start();
    accept(&server);
    let mut c = config(&server);
    c.max_queue_size = 1;
    let events = observe(&mut c);
    let client = Client::init_blocking(c);
    let _ = client.get_feature_flag("a", None);
    for _ in 0..2 {
        let _ = client.get_feature_flag("b", None);
    }
    assert_eq!(events.lock().unwrap().len(), 3);
    client.flush_blocking();
    let _ = client.get_feature_flag("b", None);
    let _ = client.get_feature_flag("b", None);
    assert_eq!(events.lock().unwrap().len(), 4);
}

#[test]
fn concurrent_duplicate_reads_do_not_wait_for_user_hooks() {
    let server = MockServer::start();
    accept(&server);
    let mut c = config(&server);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let hooks = Arc::new(AtomicUsize::new(0));
    let seen = hooks.clone();
    c.before_send(move |event| {
        seen.fetch_add(1, Ordering::SeqCst);
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        Some(event)
    });
    let client = Arc::new(Client::init_blocking(c));
    let first = client.clone();
    let first = std::thread::spawn(move || first.get_feature_flag("a", None));
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let second = client.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let second =
        std::thread::spawn(move || done_tx.send(second.get_feature_flag("a", None)).unwrap());
    let duplicate = done_rx.recv_timeout(Duration::from_secs(1));
    release_tx.send(()).unwrap();
    assert_eq!(first.join().unwrap(), Some(FlagValue::Boolean(true)));
    second.join().unwrap();
    assert_eq!(duplicate.unwrap(), Some(FlagValue::Boolean(true)));
    let _ = client.get_feature_flag_result("a", None);
    assert_eq!(hooks.load(Ordering::SeqCst), 1);
}

#[test]
fn reentrant_reads_and_opt_out_keep_return_and_outer_capture_coherent() {
    let server = MockServer::start();
    let wire = server.mock(|when, then| {
        when.path("/i/v1/analytics/events")
            .body_includes("\"distinct_id\":\"device\"")
            .body_includes("\"$feature_flag_payload\":null")
            .body_includes("\"$feature_flag_bootstrapped_payload\":null");
        then.status(200).json_body(json!({"results":{}}));
    });
    let mut c = config(&server);
    let weak = Arc::new(Mutex::new(Weak::<Client>::new()));
    let inside = weak.clone();
    let hooks = Arc::new(AtomicUsize::new(0));
    let seen = hooks.clone();
    c.before_send(move |event| {
        let client = inside.lock().unwrap().upgrade().unwrap();
        seen.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            client.get_feature_flag("a", None),
            Some(FlagValue::Boolean(true))
        );
        assert!(client.get_feature_flag_result("b", None).is_some());
        client.opt_out();
        Some(event)
    });
    let client = Arc::new(Client::init_blocking(c));
    *weak.lock().unwrap() = Arc::downgrade(&client);
    assert_eq!(
        client.get_feature_flag("a", None),
        Some(FlagValue::Boolean(true))
    );
    assert!(client.is_opt_out());
    client.flush_blocking();
    wire.assert_calls(1);
    client.opt_in();
    let _ = client.get_feature_flag("a", None);
    assert_eq!(hooks.load(Ordering::SeqCst), 1);
    let _ = client.get_feature_flag("b", None);
    assert_eq!(hooks.load(Ordering::SeqCst), 2);
}

#[test]
fn partial_reload_preserves_per_record_metadata_and_ends_bootstrap_globally() {
    let server = MockServer::start();
    accept(&server);
    let mut c = config(&server);
    let events = observe(&mut c);
    let client = Client::init_blocking(c);
    let before = client.get_feature_flag_result("a", None).unwrap();
    client.set_person_properties_for_flags(
        HashMap::from([("plan".into(), json!("pro"))]),
        Some(false),
    );
    let flags = server.mock(|when, then| {
        when.path("/flags/"); then.status(200).json_body(json!({"featureFlags":{"b":false},"requestId":"partial","errorsWhileComputingFlags":true}));
    });
    client.reload_feature_flags();
    client.flush_blocking();
    let retained = client.get_feature_flag_result("a", None).unwrap();
    let current = client.get_feature_flag_result("b", None).unwrap();
    assert_eq!(retained, before);
    assert!(retained.context.person_properties.is_empty());
    assert_eq!(retained.request_id, None);
    assert_eq!(current.context.person_properties["plan"], "pro");
    assert_eq!(current.request_id.as_deref(), Some("partial"));
    assert!(current.partial);
    assert!(!current.enabled);
    assert_eq!(current.payload, None);
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].properties()["$used_bootstrap_value"], true);
    assert_eq!(events[1].properties()["$used_bootstrap_value"], false);
    assert!(!events[1]
        .properties()
        .contains_key("$feature_flag_request_id"));
    assert_eq!(
        events[2].properties()["$feature_flag_request_id"],
        "partial"
    );
    flags.assert_calls(1);
}

#[test]
fn reset_or_shutdown_from_hook_does_not_deadlock_or_replace_returned_result() {
    for close in [false, true] {
        let server = MockServer::start();
        accept(&server);
        server.mock(|when, then| {
            when.path("/flags/");
            then.status(200).json_body(json!({"featureFlags":{}}));
        });
        let mut c = config(&server);
        let weak = Arc::new(Mutex::new(Weak::<Client>::new()));
        let inside = weak.clone();
        c.before_send(move |event| {
            let client = inside.lock().unwrap().upgrade().unwrap();
            if close {
                assert_eq!(
                    client.shutdown_blocking(None).delivery,
                    DeliveryOutcome::Deferred
                );
            } else {
                client.reset();
            }
            Some(event)
        });
        let client = Arc::new(Client::init_blocking(c));
        *weak.lock().unwrap() = Arc::downgrade(&client);
        let result = client.get_feature_flag_result("a", None).unwrap();
        assert_eq!(result.context.distinct_id, "device");
        assert!(result.enabled);
        assert_eq!(client.get_feature_flag("a", None), None);
    }
}

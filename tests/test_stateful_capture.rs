#![cfg(feature = "stateful")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use httpmock::{Method::POST, MockServer};
use posthog::stateful::{Capture, CaptureOutcome, Client, Config, DeliveryOutcome, Persistence};
use serde_json::json;
use uuid::Uuid;

fn config() -> Config {
    let mut config = Config::new("phc_test");
    config.preload_feature_flags = false;
    config.persistence = Persistence::Memory;
    config.host = "http://127.0.0.1:1".into();
    config.flush_at = 1000;
    config.capture_compression = None;
    config.max_capture_attempts = 1;
    config.shutdown_timeout = Duration::ZERO;
    config
}

#[test]
fn public_capture_and_screen_share_enrichment_hooks_and_wire_delivery() {
    let server = MockServer::start();
    let id = Uuid::now_v7();
    let screen_id = Uuid::now_v7();
    let delivered = server.mock(|when, then| {
        when.method(POST)
            .body_includes("\"event\":\"clicked\"")
            .body_includes("\"event\":\"$screen\"")
            .body_includes("\"$screen_name\":\"Settings\"")
            .body_excludes("wrong-screen")
            .body_includes("\"$lib\":\"test-sdk\"")
            .body_includes("\"$lib_version\":\"1.2.3\"")
            .body_includes("\"distinct_id\":\"seeded-anonymous\"")
            .body_includes("\"process_person_profile\":false")
            .body_includes("\"items\":[null,{}]")
            .body_excludes("hook_null")
            .body_excludes("$is_server");
        then.status(200).json_body(json!({"results": {
            (id.to_string()): {"result":"ok"},
            (screen_id.to_string()): {"result":"ok"}
        }}));
    });
    let hooks = Arc::new(AtomicUsize::new(0));
    let observed = hooks.clone();
    let mut config = config();
    config.host = server.base_url();
    config.bootstrap_anonymous_id = Some("seeded-anonymous".into());
    config.sdk_name = "test-sdk".into();
    config.sdk_version = "1.2.3".into();
    config.before_send(move |mut event| {
        observed.fetch_add(1, Ordering::SeqCst);
        if event.event_name() == "$screen" {
            assert_eq!(event.properties()["$screen_name"], "Settings");
            event.set_uuid(screen_id);
        }
        event
            .insert_prop("hook_null", serde_json::Value::Null)
            .unwrap();
        event
            .insert_prop("items", json!([null, {"drop":null}]))
            .unwrap();
        Some(event)
    });
    let client = Client::init_blocking(config);
    let mut input = Capture::new("clicked");
    input.uuid = Some(id);
    assert_eq!(client.capture(input), CaptureOutcome::Accepted { uuid: id });
    assert_eq!(
        client.screen(
            "Settings",
            HashMap::from([("$screen_name".into(), "wrong-screen".into())])
        ),
        CaptureOutcome::Accepted { uuid: screen_id }
    );
    assert_eq!(client.flush_blocking().delivery, DeliveryOutcome::Attempted);
    client.shutdown_blocking(None);
    delivered.assert_calls(1);
    assert_eq!(hooks.load(Ordering::SeqCst), 2);
}

#[test]
fn disabled_and_closed_calls_do_not_convert_inputs_or_run_hooks() {
    struct Input;
    impl From<Input> for Capture {
        fn from(_: Input) -> Self {
            panic!("disabled capture converted input")
        }
    }
    struct Name;
    impl From<Name> for String {
        fn from(_: Name) -> Self {
            panic!("disabled screen converted name")
        }
    }
    let mut disabled = config();
    disabled.disabled = true;
    disabled.before_send(|_| panic!("disabled hook ran"));
    let client = Client::init_blocking(disabled);
    assert_eq!(client.capture(Input), CaptureOutcome::Disabled);
    assert_eq!(
        client.screen(Name, HashMap::new()),
        CaptureOutcome::Disabled
    );
    let client = Client::init_blocking(config());
    client.shutdown_blocking(None);
    assert_eq!(client.capture(Input), CaptureOutcome::Closed);
    assert_eq!(client.screen(Name, HashMap::new()), CaptureOutcome::Closed);
}

#[test]
fn invalid_and_filtered_events_never_reach_delivery() {
    let server = MockServer::start();
    let request = server.mock(|when, then| {
        when.method(POST);
        then.status(500);
    });
    let mut config = config();
    config.host = server.base_url();
    config.before_send(|_| None);
    let client = Client::init_blocking(config);
    assert_eq!(client.capture(" "), CaptureOutcome::Invalid);
    assert_eq!(client.screen("", HashMap::new()), CaptureOutcome::Invalid);
    assert_eq!(client.capture("filtered"), CaptureOutcome::Dropped);
    client.flush_blocking();
    client.shutdown_blocking(None);
    request.assert_calls(0);
}

#[test]
fn bounded_admission_rejects_new_work_and_recovers_a_failed_byte_reservation() {
    let mut config = config();
    config.max_queue_size = 1;
    config.max_queue_bytes = 1024;
    config.max_record_bytes = 1024;
    let client = Client::init_blocking(config);
    let mut oversized = Capture::new("too-big");
    oversized
        .properties
        .insert("data".into(), "x".repeat(2048).into());
    assert_eq!(client.capture(oversized), CaptureOutcome::Full);
    assert!(matches!(
        client.capture("accepted"),
        CaptureOutcome::Accepted { .. }
    ));
    assert_eq!(client.capture("new"), CaptureOutcome::Full);
    client.shutdown_blocking(None);
}

#[test]
fn hooks_can_read_identity_but_recursive_capture_and_waiting_are_deferred() {
    let slot = Arc::new(Mutex::new(Weak::<Client>::new()));
    let hook_slot = slot.clone();
    let mut config = config();
    config.before_send(move |event| {
        let client = hook_slot.lock().unwrap().upgrade().unwrap();
        assert_eq!(client.get_distinct_id(), event.distinct_id());
        assert_eq!(client.capture("recursive"), CaptureOutcome::Reentrant);
        assert_eq!(
            client.screen("recursive", HashMap::new()),
            CaptureOutcome::Reentrant
        );
        assert_eq!(client.flush_blocking().delivery, DeliveryOutcome::Deferred);
        Some(event)
    });
    let client = Arc::new(Client::init_blocking(config));
    *slot.lock().unwrap() = Arc::downgrade(&client);
    assert!(matches!(
        client.capture("outer"),
        CaptureOutcome::Accepted { .. }
    ));
    client.shutdown_blocking(None);
}

#[test]
fn closing_from_a_hook_prevents_the_outer_capture_from_being_admitted() {
    let slot = Arc::new(Mutex::new(Weak::<Client>::new()));
    let hook_slot = slot.clone();
    let mut config = config();
    config.before_send(move |event| {
        let client = hook_slot.lock().unwrap().upgrade().unwrap();
        assert_eq!(
            client.shutdown_blocking(None).delivery,
            DeliveryOutcome::Deferred
        );
        Some(event)
    });
    let client = Arc::new(Client::init_blocking(config));
    *slot.lock().unwrap() = Arc::downgrade(&client);
    assert_eq!(client.capture("outer"), CaptureOutcome::Closed);
    assert_eq!(
        client.shutdown_blocking(None).delivery,
        DeliveryOutcome::Stopped
    );
}

#[test]
fn concurrent_captures_use_one_identity_and_fresh_uuids() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut config = config();
    config.bootstrap_anonymous_id = Some("shared-anonymous".into());
    config.before_send(move |event| {
        assert_eq!(event.distinct_id(), "shared-anonymous");
        observed.fetch_add(1, Ordering::SeqCst);
        Some(event)
    });
    let client = Arc::new(Client::init_blocking(config));
    let workers: Vec<_> = (0..20)
        .map(|_| {
            let client = client.clone();
            std::thread::spawn(move || client.capture("concurrent"))
        })
        .collect();
    let ids: std::collections::HashSet<_> = workers
        .into_iter()
        .map(|worker| match worker.join().unwrap() {
            CaptureOutcome::Accepted { uuid } => uuid,
            outcome => panic!("unexpected admission: {:?}", outcome),
        })
        .collect();
    assert_eq!(ids.len(), 20);
    assert_eq!(calls.load(Ordering::SeqCst), 20);
    client.shutdown_blocking(None);
}

#[test]
fn a_panicking_hook_drops_only_that_capture_and_releases_the_recursion_guard() {
    let mut config = config();
    config.max_queue_size = 1;
    let mut calls = 0;
    config.before_send(move |event| {
        calls += 1;
        if calls == 1 {
            panic!("test privacy hook failure");
        }
        Some(event)
    });
    let client = Client::init_blocking(config);
    assert_eq!(client.capture("first"), CaptureOutcome::Dropped);
    assert!(matches!(
        client.capture("second"),
        CaptureOutcome::Accepted { .. }
    ));
    client.shutdown_blocking(None);
}

#[cfg(feature = "async-client")]
#[test]
fn ambient_server_request_context_does_not_override_client_state_or_options() {
    let mut config = config();
    config.bootstrap_anonymous_id = Some("client-anonymous".into());
    config.before_send(|event| {
        assert_eq!(event.distinct_id(), "client-anonymous");
        assert!(event.properties().get("request_property").is_none());
        let session = event.properties()["$session_id"].as_str().unwrap();
        assert_ne!(session, "server-request-session");
        assert_eq!(Uuid::parse_str(session).unwrap().get_version_num(), 7);
        assert_eq!(event.options()["process_person_profile"], false);
        Some(event)
    });
    let client = Client::init_blocking(config);
    let context = posthog::RequestContext::new()
        .distinct_id("server-request-id")
        .session_id("server-request-session")
        .property("request_property", "server-only")
        .unwrap()
        .option("process_person_profile", true)
        .unwrap();
    let result =
        futures::executor::block_on(context.scope(async { client.capture("client-event") }));
    assert!(matches!(result, CaptureOutcome::Accepted { .. }));
    client.shutdown_blocking(None);
}

#[test]
fn capture_and_shutdown_are_safe_during_thread_local_destruction() {
    use std::cell::RefCell;
    struct OnExit(Option<Box<dyn FnOnce()>>);
    impl Drop for OnExit {
        fn drop(&mut self) {
            self.0.take().unwrap()();
        }
    }
    thread_local! {
        static ON_EXIT: RefCell<Option<OnExit>> = const { RefCell::new(None) };
    }
    let client = Arc::new(Client::init_blocking(config()));
    let thread_client = client.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let exit_client = thread_client.clone();
        // Initialized first, so this destructor runs after the capture guard's
        // thread-local storage is destroyed. Keep an external owner for cleanup
        // even when probing a failure in the shutdown path.
        ON_EXIT.with(|slot| {
            *slot.borrow_mut() = Some(OnExit(Some(Box::new(move || {
                let capture = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    exit_client.capture("during-exit")
                }));
                let shutdown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    exit_client.shutdown_blocking(None)
                }));
                tx.send((capture.ok(), shutdown.ok())).unwrap();
            }))))
        });
        assert_eq!(thread_client.capture(" "), CaptureOutcome::Invalid);
    })
    .join()
    .unwrap();
    let (capture, shutdown) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    client.shutdown_blocking(None);
    assert_eq!(capture, Some(CaptureOutcome::Dropped));
    assert_eq!(
        shutdown.map(|report| report.delivery),
        Some(DeliveryOutcome::Stopped)
    );
}

#[test]
fn invalid_sdk_configuration_disables_before_capture() {
    for invalid in ["", " \t ", "bad\nvalue"] {
        let mut config = config();
        config.sdk_name = invalid.into();
        let client = Client::init_blocking(config);
        assert!(client.is_disabled());
        assert_eq!(client.capture("event"), CaptureOutcome::Disabled);
    }
}

#[cfg(feature = "async-client")]
#[test]
fn async_initialization_and_sync_capture_work_without_a_tokio_runtime() {
    let client = futures::executor::block_on(Client::init(config()));
    assert!(matches!(
        client.capture("runtime-neutral"),
        CaptureOutcome::Accepted { .. }
    ));
    client.shutdown_blocking(None);
}

#![cfg(feature = "stateful")]

use httpmock::MockServer;
use posthog::stateful::CaptureOutcome;
use posthog::stateful::{
    Client, Config, DeliveryOutcome, FeatureFlagLoadError, FeatureFlagsSubscription,
    FeatureFlagsUpdate, Persistence, PersonProfiles,
};
use posthog::FlagValue;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(5);
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
    c.bootstrap_feature_flag_payloads = Some(HashMap::from([("a".into(), Value::Null)]));
    c.max_capture_attempts = 1;
    c.shutdown_timeout = Duration::from_secs(1);
    c
}
fn update(rx: &mpsc::Receiver<FeatureFlagsUpdate>) -> FeatureFlagsUpdate {
    rx.recv_timeout(WAIT).unwrap()
}
fn load(
    client: &Client,
    server: &MockServer,
    rx: &mpsc::Receiver<FeatureFlagsUpdate>,
    status: u16,
    body: Value,
) -> FeatureFlagsUpdate {
    let mut mock = server.mock(|when, then| {
        when.path("/flags/");
        then.status(status).json_body(body);
    });
    client.reload_feature_flags();
    let result = update(rx);
    mock.assert_calls(1);
    mock.delete();
    result
}

#[test]
fn setup_runtime_order_panic_isolation_drop_and_silent_opted_out_reads() {
    let server = MockServer::start();
    let mut c = config(&server);
    c.opt_out = true;
    c.person_profiles = PersonProfiles::Never;
    let events = Arc::new(AtomicUsize::new(0));
    let seen = events.clone();
    c.before_send(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
        None
    });
    let (tx, rx) = mpsc::channel();
    let first = tx.clone();
    c.on_feature_flags(move |data| {
        first.send((1, data.clone())).unwrap();
        panic!("isolated listener panic");
    });
    let second = tx.clone();
    c.on_feature_flags(move |data| {
        second.send((2, data.clone())).unwrap();
    });
    let client = Client::init_blocking(c);
    for id in [1, 2] {
        let (actual, mut data) = rx.recv_timeout(WAIT).unwrap();
        assert_eq!(actual, id);
        assert!(!data.errors_loading);
        assert_eq!(data.context.distinct_id, "device");
        let a = data.flags.as_mut().unwrap().get_mut("a").unwrap();
        assert_eq!(a.payload, Some(Value::Null));
        a.payload = Some(json!("changed copy"));
    }
    assert_eq!(
        client
            .feature_flag("a")
            .send_event(false)
            .get()
            .and_then(|flag| flag.payload),
        Some(Value::Null)
    );
    drop(client.on_feature_flags(move |data| {
        tx.send((3, data.clone())).unwrap();
    }));
    assert_eq!(rx.recv_timeout(WAIT).unwrap().0, 3);
    let flags = server.mock(|when, then| {
        when.path("/flags/");
        then.status(200)
            .json_body(json!({"featureFlags":{"a":false,"b":"green"},"requestId":"remote"}));
    });
    client.reload_feature_flags();
    for id in [1, 2, 3] {
        let (actual, data) = rx.recv_timeout(WAIT).unwrap();
        assert_eq!(actual, id);
        let a = &data.flags.unwrap()["a"];
        assert!(!a.enabled);
        assert_eq!(a.variant, None);
        assert_eq!(a.request_id.as_deref(), Some("remote"));
    }
    flags.assert_calls(1);
    assert_eq!(events.load(Ordering::SeqCst), 0);
    assert_eq!(client.peek_session_id(), None);
    client.shutdown_blocking(None);
}

#[test]
fn failures_partial_provenance_malformed_payloads_and_known_empty_are_not_conflated() {
    let server = MockServer::start();
    let client = Client::init_blocking(config(&server));
    let (tx, rx) = mpsc::channel();
    client.on_feature_flags(move |data| {
        tx.send(data.clone()).unwrap();
    });
    let seed = update(&rx);
    let failed = load(&client, &server, &rx, 503, json!({}));
    assert_eq!(failed.error, Some(FeatureFlagLoadError::Http(503)));
    assert!(failed.errors_loading);
    assert_eq!(failed.flags, seed.flags);
    let quota = load(&client, &server, &rx, 200, json!({"quotaLimited":true}));
    assert_eq!(quota.error, Some(FeatureFlagLoadError::QuotaLimited));
    assert_eq!(quota.flags, seed.flags);
    client.set_person_properties_for_flags(
        HashMap::from([("role".into(), json!("new"))]),
        Some(false),
    );
    let partial = load(
        &client,
        &server,
        &rx,
        200,
        json!({"featureFlags":{"a":false},"errorsWhileComputingFlags":true,"requestId":"partial"}),
    );
    assert!(partial.errors_loading && partial.partial && !partial.invalid_payloads);
    assert_eq!(partial.error, None);
    assert_eq!(partial.context.person_properties["role"], "new");
    let flags = partial.flags.unwrap();
    assert_eq!(flags["a"].context.person_properties["role"], "new");
    assert_eq!(flags["a"].request_id.as_deref(), Some("partial"));
    assert!(flags["b"].context.person_properties.is_empty());
    assert_eq!(flags["b"].request_id, None);
    client.set_person_properties_for_flags(
        HashMap::from([("role".into(), json!("later"))]),
        Some(false),
    );
    let (late_tx, late_rx) = mpsc::channel();
    let late = client.on_feature_flags(move |data| {
        late_tx.send(data.clone()).unwrap();
    });
    assert_eq!(update(&late_rx).context.person_properties["role"], "new");
    late.unsubscribe();
    let malformed = load(
        &client,
        &server,
        &rx,
        200,
        json!({"flags":{"a":{"key":"a","enabled":true,"metadata":{"id":1,"version":1,"payload":"invalid-json"}}}}),
    );
    assert!(malformed.errors_loading && malformed.invalid_payloads && !malformed.partial);
    assert_eq!(malformed.error, None);
    assert_eq!(malformed.flags.as_ref().unwrap()["a"].payload, None);
    assert_eq!(malformed.context.person_properties["role"], "later");
    let invalid = load(&client, &server, &rx, 200, json!({}));
    assert_eq!(invalid.error, Some(FeatureFlagLoadError::InvalidResponse));
    assert_eq!(invalid.flags, malformed.flags);
    let empty = load(&client, &server, &rx, 200, json!({"featureFlags":{}}));
    assert!(!empty.errors_loading);
    assert_eq!(empty.flags, Some(HashMap::new()));
    client.shutdown_blocking(None);
}

#[test]
fn cache_and_request_capacity_failures_notify_without_losing_usable_flags() {
    let server = MockServer::start();
    let mut c = config(&server);
    c.max_state_bytes = 4 * 1024 * 1024;
    let client = Client::init_blocking(c);
    let (tx, rx) = mpsc::channel();
    client.on_feature_flags(move |data| {
        tx.send(data.clone()).unwrap();
    });
    let seed = update(&rx);
    let rejected = load(
        &client,
        &server,
        &rx,
        200,
        json!({
            "featureFlags":{"a":true,"b":true,"c":true}, "requestId":"x".repeat(750 * 1024)
        }),
    );
    assert_eq!(rejected.error, Some(FeatureFlagLoadError::CacheTooLarge));
    assert!(rejected.errors_loading);
    assert_eq!(rejected.flags, seed.flags);
    let unexpected = server.mock(|when, then| {
        when.path("/flags/");
        then.status(503);
    });
    client.set_person_properties_for_flags(
        HashMap::from([("large".into(), json!("x".repeat(2 * 1024 * 1024)))]),
        Some(false),
    );
    client.reload_feature_flags();
    let rejected = update(&rx);
    assert_eq!(rejected.error, Some(FeatureFlagLoadError::RequestTooLarge));
    assert_eq!(rejected.flags, seed.flags);
    unexpected.assert_calls(0);
    client.shutdown_blocking(None);
}

#[test]
fn unavailable_failure_is_replayed_but_disabled_and_closed_clients_are_silent() {
    let server = MockServer::start();
    let mut c = config(&server);
    c.bootstrap_feature_flags = None;
    let client = Client::init_blocking(c);
    let (tx, rx) = mpsc::channel();
    client.on_feature_flags(move |data| {
        tx.send(data.clone()).unwrap();
    });
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let failed = load(&client, &server, &rx, 503, json!({}));
    assert_eq!(failed.flags, None);
    let (tx, rx) = mpsc::channel();
    client.on_feature_flags(move |data| {
        tx.send(data.clone()).unwrap();
    });
    assert_eq!(update(&rx), failed);
    client.shutdown_blocking(None);
    let forbidden = Arc::new(AtomicUsize::new(0));
    let calls = forbidden.clone();
    client.on_feature_flags(move |_| {
        calls.fetch_add(1, Ordering::SeqCst);
    });
    for disabled in [false, true] {
        let mut c = config(&server);
        c.disabled = disabled;
        if !disabled {
            c.project_token.clear();
        }
        let calls = forbidden.clone();
        c.on_feature_flags(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
        });
        let disabled = Client::init_blocking(c);
        assert!(disabled.is_disabled());
        let calls = forbidden.clone();
        let sub = disabled.on_feature_flags(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
        });
        sub.unsubscribe();
        sub.unsubscribe();
    }
    assert_eq!(forbidden.load(Ordering::SeqCst), 0);
}

#[test]
fn callbacks_can_unsubscribe_themselves_and_later_callbacks_idempotently() {
    let server = MockServer::start();
    let client = Client::init_blocking(config(&server));
    let subscriptions = Arc::new(Mutex::new(Vec::<FeatureFlagsSubscription>::new()));
    let slots = subscriptions.clone();
    let first_count = Arc::new(AtomicUsize::new(0));
    let count = first_count.clone();
    let first = client.on_feature_flags(move |_| {
        if count.fetch_add(1, Ordering::SeqCst) > 0 {
            for sub in slots.lock().unwrap().iter() {
                sub.unsubscribe();
                sub.unsubscribe();
            }
        }
    });
    let second_count = Arc::new(AtomicUsize::new(0));
    let count = second_count.clone();
    let second = client.on_feature_flags(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
    });
    *subscriptions.lock().unwrap() = vec![first.clone(), second];
    let (tx, rx) = mpsc::channel();
    client.on_feature_flags(move |data| {
        tx.send(data.clone()).unwrap();
    });
    update(&rx);
    load(
        &client,
        &server,
        &rx,
        200,
        json!({"featureFlags":{"a":false}}),
    );
    load(
        &client,
        &server,
        &rx,
        200,
        json!({"featureFlags":{"a":true}}),
    );
    assert_eq!(first_count.load(Ordering::SeqCst), 2);
    assert_eq!(second_count.load(Ordering::SeqCst), 1);
    first.unsubscribe();
    client.shutdown_blocking(None);
}

#[test]
fn immediate_callback_can_capture_read_flush_and_request_deferred_shutdown() {
    let server = MockServer::start();
    let mut c = config(&server);
    c.before_send(|_| None);
    let client = Arc::new(Client::init_blocking(c));
    let weak = Arc::downgrade(&client);
    let (tx, rx) = mpsc::channel();
    client.on_feature_flags(move |data| {
        let client = weak.upgrade().unwrap();
        assert_eq!(
            client.feature_flag("a").send_event(false).get(),
            Some(data.flags.as_ref().unwrap()["a"].clone())
        );
        assert_eq!(client.capture("listener-event"), CaptureOutcome::Dropped);
        assert_eq!(client.flush_blocking().delivery, DeliveryOutcome::Deferred);
        tx.send(client.shutdown_blocking(None).delivery).unwrap();
    });
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), DeliveryOutcome::Deferred);
    assert_eq!(
        client.shutdown_blocking(None).delivery,
        DeliveryOutcome::Stopped
    );
    let forbidden = Arc::new(AtomicUsize::new(0));
    let calls = forbidden.clone();
    client.on_feature_flags(move |_| {
        calls.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(forbidden.load(Ordering::SeqCst), 0);
}

#[cfg(feature = "async-client")]
#[test]
fn runtime_free_async_listener_drains_are_deferred() {
    let server = MockServer::start();
    let client = Arc::new(futures::executor::block_on(Client::init(config(&server))));
    let weak = Arc::downgrade(&client);
    let (tx, rx) = mpsc::channel();
    client.on_feature_flags(move |_| {
        let client = weak.upgrade().unwrap();
        tx.send((
            futures::executor::block_on(client.flush()).delivery,
            futures::executor::block_on(client.shutdown(None)).delivery,
        ))
        .unwrap();
    });
    assert_eq!(
        rx.recv_timeout(WAIT).unwrap(),
        (DeliveryOutcome::Deferred, DeliveryOutcome::Deferred)
    );
    assert_eq!(
        futures::executor::block_on(client.shutdown(None)).delivery,
        DeliveryOutcome::Stopped
    );
}

struct OnDrop(Option<Box<dyn FnOnce() + Send + Sync>>);
impl Drop for OnDrop {
    fn drop(&mut self) {
        if let Some(callback) = self.0.take() {
            callback();
        }
    }
}

#[test]
fn unsubscribe_drops_user_captures_outside_registry_lock() {
    let server = MockServer::start();
    let client = Client::init_blocking(config(&server));
    let other = client.on_feature_flags(|_| {});
    let (tx, rx) = mpsc::channel();
    let cleanup = OnDrop(Some(Box::new(move || {
        other.unsubscribe();
        tx.send(()).unwrap();
    })));
    let sub = client.on_feature_flags(move |_| {
        let _keep = &cleanup;
    });
    sub.unsubscribe();
    rx.recv_timeout(WAIT).unwrap();
    client.shutdown_blocking(None);
}

#[test]
fn retained_handle_does_not_keep_callbacks_or_namespace_alive_and_restore_notifies_setup() {
    let server = MockServer::start();
    let directory =
        std::env::temp_dir().join(format!("posthog-listeners-{}", uuid::Uuid::now_v7()));
    let mut c = config(&server);
    c.persistence = Persistence::Directory(directory.clone());
    let client = Client::init_blocking(c);
    let (tx, rx) = mpsc::channel();
    let cleanup = OnDrop(Some(Box::new(move || {
        tx.send(()).unwrap();
    })));
    let sub = client.on_feature_flags(move |_| {
        let _keep = &cleanup;
    });
    assert_eq!(
        client.shutdown_blocking(None).state,
        posthog::stateful::PersistenceOutcome::Persisted
    );
    rx.recv_timeout(WAIT).unwrap();
    drop(client);
    let mut c = config(&server);
    c.persistence = Persistence::Directory(directory.clone());
    c.bootstrap_feature_flags = None;
    c.bootstrap_feature_flag_payloads = None;
    let (tx, rx) = mpsc::channel();
    c.on_feature_flags(move |data| {
        tx.send(data.clone()).unwrap();
    });
    let client = Client::init_blocking(c);
    assert!(!client.is_disabled());
    let restored = update(&rx);
    assert!(restored.flags.unwrap()["a"].enabled);
    sub.unsubscribe();
    sub.unsubscribe();
    client.shutdown_blocking(None);
    drop(client);
    std::fs::remove_dir_all(directory).unwrap();
}

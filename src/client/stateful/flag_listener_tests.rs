use super::*;
use crate::stateful::{DeliveryOutcome, FeatureFlagsUpdate};
use crate::FlagValue;
use std::sync::{atomic::AtomicUsize, Barrier};

#[test]
fn setup_empty_bootstrap_notifies_before_gated_default_preload() {
    let server = Server::new(vec![(200, json!({"featureFlags":{"remote":false}}))]);
    let mut config = config(&server.host);
    config.preload_feature_flags = true;
    config.bootstrap_feature_flags = Some(HashMap::new());
    let (tx, rx) = mpsc::channel();
    config.on_feature_flags(move |data| {
        tx.send(data.clone()).unwrap();
    });
    let client = Client::init_blocking(config);
    let seed = rx.recv_timeout(WAIT).unwrap();
    assert_eq!(seed.flags, Some(HashMap::new()));
    server.request();
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    server.respond();
    assert!(!rx.recv_timeout(WAIT).unwrap().flags.unwrap()["remote"].enabled);
    client.flush_blocking();
    server.finish();
}

#[test]
fn same_context_reload_in_first_listener_does_not_starve_later_listener() {
    let server = Server::new(vec![
        (200, json!({"featureFlags":{"first":true}})),
        (200, json!({"featureFlags":{"second":false}})),
    ]);
    let client = Arc::new(Client::init_blocking(config(&server.host)));
    let weak = Arc::downgrade(&client);
    let calls = AtomicUsize::new(0);
    client.on_feature_flags(move |_| {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            weak.upgrade().unwrap().reload_feature_flags();
        }
    });
    let (tx, rx) = mpsc::channel();
    client.on_feature_flags(move |data| {
        tx.send(data.clone()).unwrap();
    });
    client.reload_feature_flags();
    server.request();
    server.respond();
    let next_request = server.request();
    let first_before_second_response = rx.try_recv();
    server.respond();
    let second = rx.recv_timeout(WAIT).unwrap();
    client.flush_blocking();
    server.finish();
    assert_eq!(next_request["distinct_id"], "device");
    let first = first_before_second_response
        .expect("queued reload must not suppress a committed notification for later listeners");
    assert!(first.flags.unwrap()["first"].enabled);
    assert!(!second.flags.unwrap()["second"].enabled);
}

#[test]
fn same_context_reload_after_commit_does_not_suppress_delayed_notification() {
    let server = Server::new(vec![
        (
            200,
            json!({"featureFlags":{"a":true,"b":true,"c":true}, "requestId":"x".repeat(750 * 1024)}),
        ),
        (200, json!({"featureFlags":{"fresh":true}})),
    ]);
    let mut config = config(&server.host);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    // Cache admission commits its failure before reporting the diagnostic.
    // Pause that report so another thread can queue a refresh before dispatch.
    config.on_error(move |_| {
        entered_tx.send(()).unwrap();
        release_rx.lock().unwrap().recv_timeout(WAIT).unwrap();
    });
    let client = Client::init_blocking(config);
    let (tx, rx) = mpsc::channel();
    client.on_feature_flags(move |data| {
        tx.send(data.clone()).unwrap();
    });
    client.reload_feature_flags();
    server.request();
    server.respond();
    entered_rx.recv_timeout(WAIT).unwrap();
    client.reload_feature_flags();
    release_tx.send(()).unwrap();
    server.request();
    let committed_before_second_response = rx.try_recv();
    server.respond();
    let fresh = rx.recv_timeout(WAIT).unwrap();
    client.flush_blocking();
    server.finish();
    let committed = committed_before_second_response
        .expect("queued reload must not suppress an already-committed load status");
    assert_eq!(
        committed.error,
        Some(crate::stateful::FeatureFlagLoadError::CacheTooLarge)
    );
    assert!(committed.errors_loading);
    assert!(fresh.flags.unwrap()["fresh"].enabled);
}

#[test]
fn reset_discards_inflight_completion_and_does_not_replay_it_to_new_listener() {
    let server = Server::new(vec![
        (200, json!({"featureFlags":{"obsolete":true}})),
        (200, json!({"featureFlags":{"fresh":false}})),
    ]);
    let mut config = config(&server.host);
    config.preload_feature_flags = true;
    let client = Client::init_blocking(config);
    let (tx, rx) = mpsc::channel();
    client.on_feature_flags(move |data| {
        tx.send(data.clone()).unwrap();
    });
    server.request();
    client.reset();
    server.respond();
    let current = server.request();
    assert_ne!(current["distinct_id"], "device");
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let (late_tx, late_rx) = mpsc::channel();
    client.on_feature_flags(move |data| {
        late_tx.send(data.clone()).unwrap();
    });
    assert!(matches!(late_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    server.respond();
    let fresh = rx.recv_timeout(WAIT).unwrap();
    assert_eq!(fresh, late_rx.recv_timeout(WAIT).unwrap());
    assert_eq!(fresh.context.distinct_id, client.get_distinct_id());
    assert_eq!(
        fresh.flags.unwrap().keys().collect::<Vec<_>>(),
        vec!["fresh"]
    );
    client.flush_blocking();
    server.finish();
}

#[test]
fn reentrant_reset_preserves_registration_and_snapshot_but_skips_remaining_old_dispatch() {
    let server = Server::new(vec![
        (200, json!({"featureFlags":{"old":true}})),
        (200, json!({"featureFlags":{"new":false}})),
    ]);
    let client = Arc::new(Client::init_blocking(config(&server.host)));
    let (tx, rx) = mpsc::channel::<(u8, FeatureFlagsUpdate)>();
    let weak = Arc::downgrade(&client);
    let first_tx = tx.clone();
    let calls = AtomicUsize::new(0);
    client.on_feature_flags(move |data| {
        let client = weak.upgrade().unwrap();
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let before = data.clone();
            client.reset();
            assert_eq!(client.get_feature_flags(), None);
            assert_eq!(data, &before);
            let new_tx = first_tx.clone();
            client.on_feature_flags(move |data| {
                new_tx.send((3, data.clone())).unwrap();
            });
            assert_eq!(client.flush_blocking().delivery, DeliveryOutcome::Deferred);
        }
        first_tx.send((1, data.clone())).unwrap();
    });
    client.on_feature_flags(move |data| {
        tx.send((2, data.clone())).unwrap();
    });
    client.reload_feature_flags();
    server.request();
    server.respond();
    let first = rx.recv_timeout(WAIT).unwrap();
    assert_eq!(first.0, 1);
    assert!(first.1.flags.unwrap()["old"].enabled);
    server.request();
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    server.respond();
    for id in [1, 2, 3] {
        let (seen, data) = rx.recv_timeout(WAIT).unwrap();
        assert_eq!(seen, id);
        assert_eq!(data.context.distinct_id, client.get_distinct_id());
        assert!(!data.flags.unwrap()["new"].enabled);
    }
    client.flush_blocking();
    server.finish();
}

#[test]
fn immediate_shutdown_during_inflight_http_is_deferred_and_suppresses_late_completion() {
    let server = Server::new(vec![(200, json!({"featureFlags":{"late":true}}))]);
    let mut config = config(&server.host);
    config.bootstrap_feature_flags =
        Some(HashMap::from([("seed".into(), FlagValue::Boolean(true))]));
    let client = Arc::new(Client::init_blocking(config));
    let (tx, rx) = mpsc::channel();
    client.on_feature_flags(move |data| {
        tx.send(data.clone()).unwrap();
    });
    rx.recv_timeout(WAIT).unwrap();
    client.reload_feature_flags();
    server.request();
    let weak = Arc::downgrade(&client);
    let (closed_tx, closed_rx) = mpsc::channel();
    client.on_feature_flags(move |_| {
        closed_tx
            .send(
                weak.upgrade()
                    .unwrap()
                    .shutdown_blocking(Some(Duration::ZERO))
                    .delivery,
            )
            .unwrap();
    });
    assert_eq!(
        closed_rx.recv_timeout(WAIT).unwrap(),
        DeliveryOutcome::Deferred
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let late_calls = calls.clone();
    client.on_feature_flags(move |_| {
        late_calls.fetch_add(1, Ordering::SeqCst);
    });
    server.respond();
    client.shutdown_blocking(None);
    server.finish();
    assert!(rx.try_recv().is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(client
        .state
        .as_ref()
        .unwrap()
        .snapshot()
        .flag_load
        .completed
        .is_none());
}

#[test]
fn remote_shutdown_skips_later_listeners_and_can_be_joined_after_callback_returns() {
    let server = Server::new(vec![(200, json!({"featureFlags":{"loaded":true}}))]);
    let client = Arc::new(Client::init_blocking(config(&server.host)));
    let weak = Arc::downgrade(&client);
    let (tx, rx) = mpsc::channel();
    client.on_feature_flags(move |_| {
        tx.send(weak.upgrade().unwrap().shutdown_blocking(None).delivery)
            .unwrap();
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let later = calls.clone();
    client.on_feature_flags(move |_| {
        later.fetch_add(1, Ordering::SeqCst);
    });
    client.reload_feature_flags();
    server.request();
    server.respond();
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), DeliveryOutcome::Deferred);
    assert_eq!(
        client.shutdown_blocking(None).delivery,
        DeliveryOutcome::Stopped
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.finish();
}

#[test]
fn blocked_remote_callback_does_not_hold_state_or_registration_locks() {
    let server = Server::new(vec![(200, json!({"featureFlags":{"ready":true}}))]);
    let client = Client::init_blocking(config(&server.host));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    client.on_feature_flags(move |_| {
        entered_tx.send(()).unwrap();
        let released = release_rx.lock().unwrap().recv_timeout(WAIT).is_ok();
        done_tx.send(released).unwrap();
    });
    client.reload_feature_flags();
    server.request();
    server.respond();
    entered_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(
        client
            .feature_flag("ready")
            .send_event(false)
            .get()
            .map(|flag| flag.enabled),
        Some(true)
    );
    client.register(HashMap::from([(
        "while-listener-blocked".into(),
        json!(true),
    )]));
    client.opt_out();
    let (tx, rx) = mpsc::channel();
    let sub = client.on_feature_flags(move |data| {
        tx.send((thread::current().id(), data.clone())).unwrap();
    });
    let (caller, data) = rx.recv_timeout(WAIT).unwrap();
    assert_eq!(caller, thread::current().id());
    assert!(data.flags.unwrap()["ready"].enabled);
    sub.unsubscribe();
    release_tx.send(()).unwrap();
    assert!(
        done_rx.recv_timeout(WAIT).unwrap(),
        "operations must finish before the callback gate times out"
    );
    client.flush_blocking();
    server.finish();
}

#[test]
fn concurrent_registration_and_completion_do_not_lose_or_duplicate_current_remote_snapshot() {
    let server = Server::new(vec![(200, json!({"featureFlags":{"fresh":true}}))]);
    let mut config = config(&server.host);
    config.bootstrap_feature_flags = Some(HashMap::new());
    let client = Arc::new(Client::init_blocking(config));
    client.reload_feature_flags();
    server.request();
    let counts = Arc::new((0..32).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>());
    let barrier = Arc::new(Barrier::new(33));
    let mut threads = Vec::new();
    for i in 0..32 {
        let client = client.clone();
        let counts = counts.clone();
        let barrier = barrier.clone();
        threads.push(thread::spawn(move || {
            barrier.wait();
            client.on_feature_flags(move |data| {
                if data
                    .flags
                    .as_ref()
                    .is_some_and(|flags| flags.contains_key("fresh"))
                {
                    counts[i].fetch_add(1, Ordering::SeqCst);
                }
            });
        }));
    }
    barrier.wait();
    server.respond();
    for task in threads {
        task.join().unwrap();
    }
    client.flush_blocking();
    for count in counts.iter() {
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
    server.finish();
}

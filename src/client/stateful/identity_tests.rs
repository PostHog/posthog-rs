use super::super::{Config, Persistence};
use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use serde_json::json;

fn config() -> Config {
    let mut config = Config::new("phc_test");
    config.persistence = Persistence::Memory;
    config.host = "http://127.0.0.1:1".into();
    config.bootstrap_anonymous_id = Some("anonymous".into());
    config.flush_at = 1000;
    config.flush_interval = Duration::from_secs(3600);
    config.shutdown_timeout = Duration::ZERO;
    config.max_capture_attempts = 1;
    config
}

#[test]
fn overlapping_property_inputs_compare_after_hooks_and_serial_repeats_skip_hooks() {
    for drop_first in [false, true] {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let hook_calls = calls.clone();
        let mut config = config();
        config.before_send(move |mut event| {
            if hook_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                if drop_first {
                    return None;
                }
            }
            event.insert_prop("$set", json!({"plan": "hook"})).unwrap();
            Some(event)
        });
        let client = Arc::new(Client::init_blocking(config));
        let set = HashMap::from([("plan".into(), json!("A"))]);
        let fingerprint = properties_hash(&set, &HashMap::new());
        // Stage two operations that both passed their early check before either
        // reaches admission, without relying on thread scheduling inside a hook.
        let (_, snapshot) = client
            .state
            .as_ref()
            .unwrap()
            .mutate(|state| {
                state.person_processing = true;
                state.clone()
            })
            .unwrap();
        let mut workers = Vec::new();
        for _ in 0..2 {
            let client = client.clone();
            let snapshot = snapshot.clone();
            let set = set.clone();
            workers.push(std::thread::spawn(move || {
                let _guard = CaptureGuard::enter(&client).unwrap();
                client.capture_person(
                    person_event("$set", set, HashMap::new()),
                    &snapshot,
                    Some(fingerprint),
                    true,
                );
            }));
        }
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        release_tx.send(()).unwrap();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(client.transport.as_ref().unwrap().pending(), 1);
        // The fingerprint remains the Rust input, not the hook-transformed payload.
        client.set_person_properties(set);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        client.set_person_properties(HashMap::from([("plan".into(), json!("B"))]));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(client.transport.as_ref().unwrap().pending(), 2);
    }
}

#[test]
fn reset_scope_does_not_suppress_or_cache_an_old_prepared_property_event() {
    let client = Client::init_blocking(config());
    let set = HashMap::from([("plan".into(), json!("A"))]);
    let fingerprint = properties_hash(&set, &HashMap::new());
    let old = client.state.as_ref().unwrap().snapshot();
    client.reset();
    client.set_person_properties(set.clone());
    // The new scope has the same fingerprint; an old snapshot must still be
    // admitted, without replacing the new scope's cache with its next update.
    client.capture_person(
        person_event("$set", set.clone(), HashMap::new()),
        &old,
        Some(fingerprint),
        true,
    );
    let other = HashMap::from([("plan".into(), json!("B"))]);
    client.capture_person(
        person_event("$set", other.clone(), HashMap::new()),
        &old,
        Some(properties_hash(&other, &HashMap::new())),
        true,
    );
    client.set_person_properties(set);
    assert_eq!(client.transport.as_ref().unwrap().pending(), 3);
    assert_eq!(
        client.state.as_ref().unwrap().snapshot().person_properties,
        Some(fingerprint)
    );
}

#[test]
fn only_updates_that_surely_process_the_person_are_deduplicated() {
    let client = Client::init_blocking(config());
    let pending = || client.transport.as_ref().unwrap().pending();
    let update = |setting: Option<Value>| {
        let mut update = PersonProperties::new();
        update.set.insert("plan".into(), json!("pro"));
        if let Some(setting) = setting {
            update
                .options
                .insert("process_person_profile".into(), setting);
        }
        client.set_person_properties(update);
    };
    update(Some(json!(false)));
    assert_eq!(pending(), 1);
    // The personless update recorded nothing, so this one is sent.
    update(None);
    assert_eq!(pending(), 2);
    update(Some(json!(true)));
    assert_eq!(pending(), 2);
    // A value PostHog reads is never parsed: it is sent and clears the hash.
    update(Some(json!("false")));
    update(Some(json!("false")));
    assert_eq!(pending(), 4);
    update(None);
    assert_eq!(pending(), 5);

    // A hook that turns processing off is read as sent.
    let mut config = config();
    config.before_send(|mut event| {
        event
            .insert_option("process_person_profile", false)
            .unwrap();
        Some(event)
    });
    let client = Client::init_blocking(config);
    for _ in 0..2 {
        client.set_person_properties(HashMap::from([("plan".into(), json!("pro"))]));
    }
    assert_eq!(client.transport.as_ref().unwrap().pending(), 2);
}

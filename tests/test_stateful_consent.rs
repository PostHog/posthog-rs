#![cfg(feature = "stateful")]

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::Duration;

use httpmock::{Method::POST, MockServer};
use posthog::stateful::{
    CaptureOutcome, Client, Config, Identify, Persistence, PersistenceOutcome,
};
use posthog::Event;
use serde_json::{json, Value};
use uuid::Uuid;

fn config() -> Config {
    let mut conf = Config::new("phc_test");
    conf.preload_feature_flags = false;
    conf.persistence = Persistence::Memory;
    conf.host = "http://127.0.0.1:1".into();
    conf.bootstrap_anonymous_id = Some("anonymous".into());
    conf.flush_at = 1000;
    conf.flush_interval = Duration::from_secs(3600);
    conf.capture_compression = None;
    conf.shutdown_timeout = Duration::ZERO;
    conf.max_capture_attempts = 1;
    conf
}

fn observed(mut conf: Config) -> (Client, Arc<Mutex<Vec<Event>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    conf.before_send(move |event| {
        seen.lock().unwrap().push(event.clone());
        Some(event)
    });
    (Client::init_blocking(conf), events)
}

fn accepted(outcome: CaptureOutcome) {
    assert!(
        matches!(outcome, CaptureOutcome::Accepted { .. }),
        "{:?}",
        outcome
    );
}

struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("posthog-consent-{}", Uuid::now_v7())))
    }
    fn config(&self) -> Config {
        let mut conf = config();
        conf.persistence = Persistence::Directory(self.0.clone());
        conf.shutdown_timeout = Duration::from_secs(2);
        conf
    }
    fn state_path(&self) -> std::path::PathBuf {
        std::fs::read_dir(&self.0)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("state.json")
    }
    fn state(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.state_path()).unwrap()).unwrap()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn concurrent_choices_coalesce_without_reverting_the_final_revision() {
    let directory = Directory::new();
    let client = Arc::new(Client::init_blocking(directory.config()));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let client = client.clone();
            std::thread::spawn(move || {
                for _ in 0..100 {
                    client.opt_out();
                    client.opt_in();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    client.opt_out();
    assert!(client.is_opt_out());
    assert_eq!(client.flush_blocking().state, PersistenceOutcome::Persisted);
    assert!(client.is_opt_out());
    assert_eq!(directory.state()["state"]["opt_out"], true);
}

#[test]
fn toggles_are_immediate_and_blocked_capture_never_runs_hooks() {
    let (client, events) = observed(config());
    assert!(!client.is_opt_out());
    accepted(client.capture("before"));
    client.opt_out();
    client.opt_out();
    assert!(client.is_opt_out());
    assert_eq!(client.capture("blocked"), CaptureOutcome::OptedOut);
    assert_eq!(
        client.screen("blocked", HashMap::new()),
        CaptureOutcome::OptedOut
    );
    assert_eq!(events.lock().unwrap().len(), 1);
    client.opt_in();
    client.opt_in();
    assert!(!client.is_opt_out());
    accepted(client.capture("after"));
    assert_eq!(events.lock().unwrap().len(), 2);
}

#[test]
fn disabled_and_closed_clients_cannot_be_opted_in() {
    for disabled in [false, true] {
        let mut conf = config();
        conf.disabled = disabled;
        let client = Client::init_blocking(conf);
        if !disabled {
            client.shutdown_blocking(None);
        }
        client.opt_in();
        client.opt_out();
        client.reset();
        assert!(client.is_opt_out());
        assert_eq!(
            client.capture("blocked"),
            if disabled {
                CaptureOutcome::Disabled
            } else {
                CaptureOutcome::Closed
            }
        );
    }
}

#[test]
fn reset_restores_configured_default_and_clears_persisted_override() {
    for default in [false, true] {
        let directory = Directory::new();
        let mut conf = directory.config();
        conf.opt_out = default;
        let client = Client::init_blocking(conf);
        assert_eq!(client.is_opt_out(), default);
        if default {
            client.opt_in()
        } else {
            client.opt_out()
        }
        assert_eq!(client.is_opt_out(), !default);
        client.reset();
        assert_eq!(client.is_opt_out(), default);
        assert_ne!(client.get_distinct_id(), "anonymous");
        assert_eq!(
            client.shutdown_blocking(None).state,
            PersistenceOutcome::Persisted
        );
        drop(client);
        assert!(directory.state()["state"]["opt_out"].is_null());
        // Absence is not frozen to the previous launch's default.
        let mut conf = directory.config();
        conf.opt_out = !default;
        let client = Client::init_blocking(conf);
        assert_eq!(client.is_opt_out(), !default);
    }
}

#[test]
fn persisted_explicit_choice_wins_over_opposite_startup_default() {
    for choice in [false, true] {
        let directory = Directory::new();
        let client = Client::init_blocking(directory.config());
        if choice {
            client.opt_out()
        } else {
            client.opt_in()
        }
        assert_eq!(
            client.shutdown_blocking(None).state,
            PersistenceOutcome::Persisted
        );
        drop(client);
        assert_eq!(directory.state()["version"], 7);
        assert_eq!(directory.state()["state"]["opt_out"], choice);
        let mut conf = directory.config();
        conf.opt_out = !choice;
        let (client, events) = observed(conf);
        assert_eq!(client.is_opt_out(), choice);
        if choice {
            assert_eq!(client.capture("early"), CaptureOutcome::OptedOut);
            assert!(events.lock().unwrap().is_empty());
        }
    }
}

#[test]
fn identify_while_out_changes_identity_without_link_replay_or_property_dedup() {
    let (client, events) = observed(config());
    client.opt_out();
    let mut input = Identify::new("A");
    input.set.insert("plan".into(), json!("pro"));
    client.identify(input.clone());
    client.identify(input.clone());
    assert_eq!(client.get_distinct_id(), "A");
    assert!(client.is_identified());
    assert!(events.lock().unwrap().is_empty());
    client.opt_in();
    client.identify("A");
    assert!(events.lock().unwrap().is_empty());
    client.identify(input.clone());
    client.identify(input);
    let seen = events.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].event_name(), "$set");
    assert_eq!(seen[0].distinct_id(), "A");
}

#[test]
fn alias_and_profile_creation_are_noops_while_out_and_properties_remain_retryable() {
    let mut conf = config();
    conf.opt_out = true;
    let (client, events) = observed(conf);
    client.alias("alias");
    client.create_person_profile();
    client.opt_in();
    accepted(client.capture("still-anonymous"));
    assert_eq!(
        events.lock().unwrap()[0].options()["process_person_profile"],
        false
    );
    client.create_person_profile();
    assert_eq!(events.lock().unwrap()[1].event_name(), "$set");
    client.opt_out();
    let set = HashMap::from([("plan".into(), json!("pro"))]);
    client.set_person_properties(set.clone(), HashMap::new());
    assert_eq!(events.lock().unwrap().len(), 2);
    client.opt_in();
    client.set_person_properties(set.clone(), HashMap::new());
    client.set_person_properties(set, HashMap::new());
    assert_eq!(events.lock().unwrap().len(), 3);
}

#[test]
fn restored_opt_out_precedes_bootstrap_reconciliation_and_hooks() {
    for same_id in [false, true] {
        let directory = Directory::new();
        let client = Client::init_blocking(directory.config());
        client.opt_out();
        assert_eq!(
            client.shutdown_blocking(None).state,
            PersistenceOutcome::Persisted
        );
        drop(client);
        let mut conf = directory.config();
        conf.bootstrap_anonymous_id = None;
        let id = if same_id { "anonymous" } else { "A" };
        conf.bootstrap_identified_id = Some(id.into());
        let (client, events) = observed(conf);
        assert!(client.is_opt_out());
        assert!(client.is_identified());
        assert_eq!(client.get_distinct_id(), id);
        assert!(events.lock().unwrap().is_empty());
        client.opt_in();
        assert_eq!(client.get_distinct_id(), id);
        client.identify(id);
        assert!(events.lock().unwrap().is_empty());
    }
}

#[test]
fn permitted_hook_can_finish_after_opt_out_and_nested_capture_checks_new_consent() {
    for reenable in [false, true] {
        let server = MockServer::start();
        let request = server.mock(|when, then| {
            when.method(POST).body_includes("permitted");
            then.status(200);
        });
        let holder = Arc::new(Mutex::new(Weak::<Client>::new()));
        let hook_holder = holder.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut conf = config();
        conf.host = server.base_url();
        conf.before_send(move |event| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            let client = hook_holder.lock().unwrap().upgrade().unwrap();
            client.opt_out();
            assert_eq!(client.capture("nested"), CaptureOutcome::OptedOut);
            if reenable {
                client.opt_in();
            }
            Some(event)
        });
        let client = Arc::new(Client::init_blocking(conf));
        *holder.lock().unwrap() = Arc::downgrade(&client);
        let capturing = client.clone();
        let thread = std::thread::spawn(move || capturing.capture("permitted"));
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        client.opt_out();
        assert_eq!(client.capture("blocked"), CaptureOutcome::OptedOut);
        release_tx.send(()).unwrap();
        accepted(thread.join().unwrap());
        assert_eq!(client.is_opt_out(), !reenable);
        client.flush_blocking();
        request.assert_calls(1);
    }
}

#[test]
fn legacy_records_use_startup_default_without_eager_rewrite() {
    for version in [1, 2] {
        let directory = Directory::new();
        let client = Client::init_blocking(directory.config());
        client.shutdown_blocking(None);
        drop(client);
        let mut state = json!({"anonymous_id":"legacy", "distinct_id":"legacy"});
        if version == 2 {
            state["identified"] = json!(true);
            state["person_processing"] = json!(true);
        }
        let bytes = serde_json::to_vec(&json!({"version": version, "state": state})).unwrap();
        std::fs::write(directory.state_path(), &bytes).unwrap();
        let mut conf = directory.config();
        conf.opt_out = true;
        let client = Client::init_blocking(conf);
        assert_eq!(client.get_distinct_id(), "legacy");
        assert_eq!(client.is_identified(), version == 2);
        assert!(client.is_opt_out());
        assert_eq!(client.capture("early"), CaptureOutcome::OptedOut);
        assert_eq!(std::fs::read(directory.state_path()).unwrap(), bytes);
        client.opt_in();
        assert_eq!(
            client.shutdown_blocking(None).state,
            PersistenceOutcome::Persisted
        );
        drop(client);
        assert_eq!(directory.state()["version"], 7);
        assert_eq!(directory.state()["state"]["opt_out"], false);
    }
}

#[test]
fn unreadable_or_expanded_legacy_state_preserves_bytes_and_uses_safe_configured_default() {
    for legacy in [false, true] {
        let directory = Directory::new();
        let client = Client::init_blocking(directory.config());
        client.shutdown_blocking(None);
        drop(client);
        let bytes = if legacy {
            serde_json::to_vec(&json!({"version":2, "state":{
                "anonymous_id":"a".repeat(200), "distinct_id":"b".repeat(200),
                "identified":true, "person_processing":true
            }}))
            .unwrap()
        } else {
            // A version-3 record missing consent must not silently opt in.
            br#"{"version":3,"state":{"anonymous_id":"old","distinct_id":"old","identified":false,"person_processing":false}}"#.to_vec()
        };
        std::fs::write(directory.state_path(), &bytes).unwrap();
        let errors = Arc::new(Mutex::new(0));
        let seen_errors = errors.clone();
        let mut conf = directory.config();
        conf.opt_out = true;
        if legacy {
            conf.max_state_bytes = bytes.len();
        }
        conf.on_error(move |error| {
            if matches!(error, posthog::PostHogError::Storage(_)) {
                *seen_errors.lock().unwrap() += 1;
            }
        });
        let client = Client::init_blocking(conf);
        assert!(!client.is_disabled());
        assert!(client.is_opt_out());
        assert!(!client.is_identified());
        assert_eq!(client.get_distinct_id(), "anonymous");
        assert_eq!(client.capture("blocked"), CaptureOutcome::OptedOut);
        assert_eq!(
            client.flush_blocking().state,
            PersistenceOutcome::Unavailable
        );
        assert!(*errors.lock().unwrap() > 0);
        assert_eq!(std::fs::read(directory.state_path()).unwrap(), bytes);
    }
}

#[test]
fn failed_write_does_not_revert_live_consent_and_recovery_persists_latest() {
    let directory = Directory::new();
    let client = Client::init_blocking(directory.config());
    assert_eq!(client.flush_blocking().state, PersistenceOutcome::Persisted);
    let temporary = directory.state_path().with_file_name("state.json.tmp");
    std::fs::create_dir(&temporary).unwrap();
    client.opt_out();
    assert_eq!(
        client.flush_blocking().state,
        PersistenceOutcome::WriteFailed
    );
    assert!(client.is_opt_out());
    assert_eq!(client.capture("blocked"), CaptureOutcome::OptedOut);
    client.opt_in();
    client.opt_out();
    std::fs::remove_dir(temporary).unwrap();
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    drop(client);
    let client = Client::init_blocking(directory.config());
    assert!(client.is_opt_out());
}

#[test]
fn max_size_state_can_always_toggle_and_persist_consent() {
    let directory = Directory::new();
    let mut conf = directory.config();
    conf.max_state_bytes = 512;
    conf.before_send(|_| None);
    let client = Client::init_blocking(conf);
    // Find the largest admitted identity without coupling to the private schema size.
    let mut largest = String::new();
    for length in (1..600).rev() {
        let id = "x".repeat(length);
        client.identify(id.clone());
        if client.is_identified() {
            largest = id;
            break;
        }
    }
    assert!(!largest.is_empty());
    client.opt_out();
    assert!(client.is_opt_out());
    client.opt_in();
    assert!(!client.is_opt_out());
    client.opt_out();
    assert!(client.is_opt_out());
    assert_eq!(client.get_distinct_id(), largest);
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    drop(client);
    assert!(std::fs::metadata(directory.state_path()).unwrap().len() <= 512);
    let mut conf = directory.config();
    conf.max_state_bytes = 512;
    let client = Client::init_blocking(conf);
    assert_eq!(client.get_distinct_id(), largest);
    assert!(client.is_opt_out());
    client.opt_in();
    assert!(!client.is_opt_out());
}

#[test]
fn admitted_event_recovers_and_delivers_with_restored_opt_out() {
    let directory = Directory::new();
    let server = MockServer::start();
    let mut reject = server.mock(|when, then| {
        when.method(POST);
        then.status(503);
    });
    let mut conf = directory.config();
    conf.host = server.base_url();
    let client = Client::init_blocking(conf);
    // Readiness does not wait for recovery's temporary capacity reservations.
    assert_eq!(client.flush_blocking().state, PersistenceOutcome::Persisted);
    accepted(client.capture("admitted-before-opt-out"));
    client.opt_out();
    client.flush_blocking();
    assert_eq!(
        client.shutdown_blocking(Some(Duration::ZERO)).state,
        PersistenceOutcome::Persisted
    );
    drop(client);
    assert!(reject.calls() >= 1);
    reject.delete();
    let accept = server.mock(|when, then| {
        when.method(POST).body_includes("admitted-before-opt-out");
        then.status(200);
    });
    let mut conf = directory.config();
    conf.host = server.base_url();
    let client = Client::init_blocking(conf);
    assert!(client.is_opt_out());
    assert_eq!(client.capture("new-blocked"), CaptureOutcome::OptedOut);
    client.flush_blocking();
    accept.assert_calls(1);
    assert!(client.is_opt_out());
}

#[cfg(feature = "async-client")]
#[test]
fn async_initialization_and_drains_resolve_consent_without_an_application_runtime() {
    let directory = Directory::new();
    futures::executor::block_on(async {
        let mut conf = directory.config();
        conf.opt_out = true;
        let client = Client::init(conf).await;
        assert!(client.is_opt_out());
        client.opt_in();
        assert!(!client.is_opt_out());
        assert_eq!(client.flush().await.state, PersistenceOutcome::Persisted);
        client.shutdown(None).await;
    });
    let client = Client::init_blocking(directory.config());
    assert!(!client.is_opt_out());
}

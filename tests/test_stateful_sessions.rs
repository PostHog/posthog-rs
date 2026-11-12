#![cfg(feature = "stateful")]

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::Duration;

use httpmock::{Method::POST, MockServer};
use posthog::stateful::{
    Capture, CaptureOutcome, Client, Config, Persistence, PersistenceOutcome, SessionMode,
    SessionPersistence,
};
use posthog::Event;
use serde_json::{json, Value};
use uuid::Uuid;

fn config() -> Config {
    let mut config = Config::new("phc_test");
    config.preload_feature_flags = false;
    config.persistence = Persistence::Memory;
    config.host = "http://127.0.0.1:1".into();
    config.flush_at = 1000;
    config.flush_interval = Duration::from_secs(3600);
    config.shutdown_timeout = Duration::ZERO;
    config.max_capture_attempts = 1;
    config.capture_compression = None;
    config
}
fn observed(mut config: Config) -> (Client, Arc<Mutex<Vec<Event>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    config.before_send(move |event| {
        seen.lock().unwrap().push(event);
        None
    });
    (Client::init_blocking(config), events)
}
struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("posthog-sessions-{}", Uuid::now_v7())))
    }
    fn config(&self) -> Config {
        let mut conf = config();
        conf.persistence = Persistence::Directory(self.0.clone());
        conf.session_persistence = SessionPersistence::Persistent;
        conf.shutdown_timeout = Duration::from_secs(2);
        conf.before_send(|_| None);
        conf
    }
    fn path(&self) -> std::path::PathBuf {
        std::fs::read_dir(&self.0)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("state.json")
    }
    fn state(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.path()).unwrap()).unwrap()
    }
    fn replace(&self, value: &Value) {
        std::fs::write(self.path(), serde_json::to_vec(value).unwrap()).unwrap();
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn defaults_getters_capture_and_manual_boundaries() {
    let conf = config();
    assert_eq!(conf.session_mode, SessionMode::Automatic);
    assert_eq!(conf.session_persistence, SessionPersistence::Memory);
    let (client, events) = observed(conf);
    assert_eq!(client.peek_session_id(), None);
    let first = client.session_id().unwrap();
    assert_eq!(first.get_version_num(), 7);
    assert_eq!(client.session_id(), Some(first));
    client.start_session();
    assert_eq!(client.peek_session_id(), Some(first));
    client.capture("first");
    client.screen("screen", HashMap::new());
    client.reset_session();
    let next = client.peek_session_id().unwrap();
    assert_ne!(first, next);
    client.capture("next");
    let seen = events.lock().unwrap();
    assert_eq!(seen[0].properties()["$session_id"], first.to_string());
    assert_eq!(seen[1].properties()["$session_id"], first.to_string());
    assert_eq!(seen[2].properties()["$session_id"], next.to_string());
    assert_eq!(seen.len(), 3);
}

#[test]
fn manual_mode_is_sessionless_until_start_and_ends_until_restarted() {
    let mut conf = config();
    conf.session_mode = SessionMode::Manual;
    let (client, events) = observed(conf);
    client.capture("sessionless");
    client.session_activity();
    client.set_app_in_background(true);
    client.set_app_in_background(false);
    assert_eq!(client.session_id(), None);
    client.start_session();
    let id = client.session_id().unwrap();
    client.capture("active");
    client.end_session();
    client.capture("ended");
    assert_eq!(client.session_id(), None);
    client.reset_session();
    assert_ne!(client.session_id(), Some(id));
    client.reset();
    assert_eq!(client.session_id(), None);
    let seen = events.lock().unwrap();
    assert!(!seen[0].properties().contains_key("$session_id"));
    assert_eq!(seen[1].properties()["$session_id"], id.to_string());
    assert!(!seen[2].properties().contains_key("$session_id"));
}

#[test]
fn consent_disabled_closed_invalid_and_recursive_capture_do_not_create() {
    let mut conf = config();
    conf.opt_out = true;
    let (client, _) = observed(conf);
    client.start_session();
    client.reset_session();
    client.session_activity();
    assert_eq!(client.session_id(), None);
    assert_eq!(client.capture("blocked"), CaptureOutcome::OptedOut);
    client.opt_in();
    assert_eq!(client.peek_session_id(), None);
    assert_eq!(client.capture(" "), CaptureOutcome::Invalid);
    client.start_session();
    let original = client.peek_session_id();
    client.opt_out();
    client.reset_session();
    assert_eq!(client.peek_session_id(), original);
    client.end_session();
    assert_eq!(client.peek_session_id(), None);
    client.shutdown_blocking(None);
    client.start_session();
    assert_eq!(client.session_id(), None);
    let mut conf = config();
    conf.disabled = true;
    let client = Client::init_blocking(conf);
    client.start_session();
    client.reset_session();
    client.session_activity();
    assert_eq!(client.session_id(), None);
    assert_eq!(client.peek_session_id(), None);

    let slot = Arc::new(Mutex::new(Weak::<Client>::new()));
    let hook_slot = slot.clone();
    let mut conf = config();
    conf.before_send(move |event| {
        let client = hook_slot.lock().unwrap().upgrade().unwrap();
        client.end_session();
        assert_eq!(client.capture("recursive"), CaptureOutcome::Reentrant);
        assert_eq!(client.peek_session_id(), None);
        Some(event)
    });
    let client = Arc::new(Client::init_blocking(conf));
    *slot.lock().unwrap() = Arc::downgrade(&client);
    client.capture("outer");
}

#[test]
fn background_state_survives_identity_reset_and_passive_helpers_are_sessionless() {
    let (client, events) = observed(config());
    client.group("company", "a", None);
    client.identify("person");
    assert_eq!(client.peek_session_id(), None);
    client.set_app_in_background(true);
    client.capture("background");
    assert_eq!(client.session_id(), None);
    client.reset();
    client.capture("still background");
    assert_eq!(client.peek_session_id(), None);
    client.set_app_in_background(false);
    assert!(client.peek_session_id().is_some());
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .all(|event| !event.properties().contains_key("$session_id")));
}

#[test]
fn canonical_event_registered_and_platform_session_precedence() {
    let mut conf = config();
    conf.platform_context
        .insert("$session_id".into(), json!("platform"));
    let (client, events) = observed(conf);
    let managed = client.session_id().unwrap();
    client.capture("platform");
    client.register(HashMap::from([("$session_id".into(), json!("registered"))]));
    client.capture("registered");
    let mut event = Capture::new("event");
    event
        .properties
        .insert("$session_id".into(), json!("event"));
    client.capture(event);
    assert_eq!(client.peek_session_id(), Some(managed));
    let seen = events.lock().unwrap();
    for (index, value) in ["platform", "registered", "event"].iter().enumerate() {
        assert_eq!(seen[index].properties()["$session_id"], *value);
    }
}

#[test]
fn persistent_session_continues_but_memory_mode_clears_disk_session() {
    let directory = Directory::new();
    let client = Client::init_blocking(directory.config());
    let id = client.session_id().unwrap();
    client.capture("touch");
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    drop(client);
    assert_eq!(directory.state()["version"], 7);
    assert_eq!(directory.state()["state"]["session"]["id"], id.to_string());
    let client = Client::init_blocking(directory.config());
    assert_eq!(client.peek_session_id(), Some(id));
    client.shutdown_blocking(None);
    drop(client);
    let mut conf = directory.config();
    conf.session_persistence = SessionPersistence::Memory;
    let client = Client::init_blocking(conf);
    assert_eq!(client.peek_session_id(), None);
    assert_ne!(client.session_id(), Some(id));
    client.register(HashMap::from([("plan".into(), json!("pro"))]));
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    drop(client);
    assert_eq!(directory.state()["state"]["session"], Value::Null);
    let client = Client::init_blocking(directory.config());
    assert_eq!(client.peek_session_id(), None);
    assert_ne!(client.session_id(), Some(id));
}

#[test]
fn persistent_choice_does_not_override_memory_backend_and_clients_are_independent() {
    let mut conf = config();
    conf.session_persistence = SessionPersistence::Persistent;
    let client = Client::init_blocking(conf);
    let first = client.session_id();
    let second = Client::init_blocking(config());
    assert_ne!(first, second.session_id());
    assert_eq!(
        client.flush_blocking().state,
        PersistenceOutcome::MemoryOnly
    );
}

#[test]
fn reset_clears_persisted_session_and_retains_manual_policy() {
    let directory = Directory::new();
    let mut conf = directory.config();
    conf.session_mode = SessionMode::Manual;
    let client = Client::init_blocking(conf);
    client.start_session();
    assert!(client.peek_session_id().is_some());
    client.reset();
    assert_eq!(client.session_id(), None);
    client.shutdown_blocking(None);
    drop(client);
    assert_eq!(directory.state()["state"]["session"], Value::Null);
}

#[test]
fn legacy_v4_migrates_lazily_and_invalid_session_files_are_preserved() {
    let directory = Directory::new();
    let client = Client::init_blocking(directory.config());
    client.shutdown_blocking(None);
    drop(client);
    let mut legacy = directory.state();
    legacy["version"] = json!(4);
    legacy["state"].as_object_mut().unwrap().remove("session");
    legacy["state"].as_object_mut().unwrap().remove("flags");
    legacy["state"]
        .as_object_mut()
        .unwrap()
        .remove("project_config");
    directory.replace(&legacy);
    let bytes = std::fs::read(directory.path()).unwrap();
    let client = Client::init_blocking(directory.config());
    assert_eq!(client.peek_session_id(), None);
    assert_eq!(client.flush_blocking().state, PersistenceOutcome::Persisted);
    assert_eq!(std::fs::read(directory.path()).unwrap(), bytes);
    client.session_activity();
    client.shutdown_blocking(None);
    drop(client);
    let valid = directory.state();
    assert_eq!(valid["version"], 7);
    for invalid in [
        json!("bad"),
        json!({"id":"not-a-uuid","started_ms":1,"activity_ms":2}),
        json!({"id":Uuid::now_v7(),"started_ms":3,"activity_ms":2}),
    ] {
        let mut value = valid.clone();
        value["state"]["session"] = invalid;
        directory.replace(&value);
        let bytes = std::fs::read(directory.path()).unwrap();
        let client = Client::init_blocking(directory.config());
        assert!(!client.is_disabled());
        assert_eq!(client.peek_session_id(), None);
        assert!(client.session_id().is_some());
        assert_eq!(
            client.shutdown_blocking(None).state,
            PersistenceOutcome::Unavailable
        );
        drop(client);
        assert_eq!(std::fs::read(directory.path()).unwrap(), bytes);
    }
}

#[test]
fn expired_or_future_wall_timing_is_not_restored_but_identity_survives() {
    let directory = Directory::new();
    let client = Client::init_blocking(directory.config());
    client.identify("person");
    client.start_session();
    client.shutdown_blocking(None);
    drop(client);
    let original = directory.state();
    for (started, activity) in [(1u64, 1u64), (u64::MAX, u64::MAX)] {
        let mut value = original.clone();
        value["state"]["session"]["started_ms"] = json!(started);
        value["state"]["session"]["activity_ms"] = json!(activity);
        directory.replace(&value);
        let client = Client::init_blocking(directory.config());
        assert_eq!(client.get_distinct_id(), "person");
        assert_eq!(client.peek_session_id(), None);
        client.shutdown_blocking(None);
        drop(client);
        assert_eq!(directory.state()["state"]["session"], Value::Null);
    }
}

#[test]
fn full_state_reserves_session_rotation_activity_and_consent_capacity() {
    let directory = Directory::new();
    let mut conf = directory.config();
    conf.max_state_bytes = 512;
    let client = Client::init_blocking(conf);
    assert!(!client.is_disabled());
    for length in (1..512).rev() {
        client.identify("x".repeat(length));
        if client.is_identified() {
            break;
        }
    }
    assert!(client.is_identified());
    let person = client.get_distinct_id();
    let first = client.session_id().unwrap();
    client.session_activity();
    client.reset_session();
    assert_ne!(client.peek_session_id(), Some(first));
    let id = client.peek_session_id();
    client.opt_out();
    assert!(client.is_opt_out());
    client.opt_in();
    client.opt_out();
    assert_eq!(client.get_distinct_id(), person);
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    drop(client);
    assert!(std::fs::metadata(directory.path()).unwrap().len() <= 512);
    let mut conf = directory.config();
    conf.max_state_bytes = 512;
    let client = Client::init_blocking(conf);
    assert_eq!(client.peek_session_id(), id);
    assert!(client.is_opt_out());
    assert_eq!(client.get_distinct_id(), person);
    client.end_session();
    assert_eq!(client.peek_session_id(), None);
}

#[test]
fn session_state_write_failure_preserves_live_session_and_retries() {
    let directory = Directory::new();
    let client = Client::init_blocking(directory.config());
    client.flush_blocking();
    let path = directory.path();
    let temp = path.with_file_name("state.json.tmp");
    std::fs::create_dir(&temp).unwrap();
    let id = client.session_id().unwrap();
    assert_eq!(
        client.flush_blocking().state,
        PersistenceOutcome::WriteFailed
    );
    assert_eq!(client.peek_session_id(), Some(id));
    client.reset_session();
    let next = client.peek_session_id();
    std::fs::remove_dir(temp).unwrap();
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    drop(client);
    let client = Client::init_blocking(directory.config());
    assert_eq!(client.peek_session_id(), next);
}

#[test]
fn queued_wire_and_hook_snapshot_keep_old_session_across_reset() {
    let server = MockServer::start();
    let mut conf = config();
    conf.host = server.base_url();
    let slot = Arc::new(Mutex::new(Weak::<Client>::new()));
    let hook_slot = slot.clone();
    conf.before_send(move |event| {
        if event.event_name() == "old" {
            hook_slot.lock().unwrap().upgrade().unwrap().reset();
        }
        Some(event)
    });
    let client = Arc::new(Client::init_blocking(conf));
    *slot.lock().unwrap() = Arc::downgrade(&client);
    let old_id = client.session_id().unwrap();
    let old_person = client.get_distinct_id();
    let old = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .body_includes(old_id.to_string())
            .body_includes(old_person);
        then.status(200);
    });
    assert!(matches!(
        client.capture("old"),
        CaptureOutcome::Accepted { .. }
    ));
    assert_eq!(client.peek_session_id(), None);
    client.flush_blocking();
    old.assert_calls(1);
    assert_ne!(client.session_id(), Some(old_id));
}

#[test]
fn capture_racing_reset_keeps_coherent_pre_hook_identity_and_session() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut conf = config();
    let seen = Arc::new(Mutex::new(None));
    let hook_seen = seen.clone();
    conf.before_send(move |event| {
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        *hook_seen.lock().unwrap() = Some(event);
        None
    });
    let client = Arc::new(Client::init_blocking(conf));
    let id = client.session_id().unwrap();
    let person = client.get_distinct_id();
    let other = client.clone();
    let caller = std::thread::spawn(move || other.capture("old"));
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    client.reset();
    let next = client.session_id().unwrap();
    release_tx.send(()).unwrap();
    caller.join().unwrap();
    assert_ne!(next, id);
    let seen = seen.lock().unwrap();
    let event = seen.as_ref().unwrap();
    assert_eq!(event.distinct_id(), person);
    assert_eq!(event.properties()["$session_id"], id.to_string());
}

#[test]
fn full_admission_retains_activity_and_recovered_wire_keeps_original_session() {
    let mut conf = config();
    conf.max_queue_size = 1;
    let client = Client::init_blocking(conf);
    assert!(matches!(
        client.capture("first"),
        CaptureOutcome::Accepted { .. }
    ));
    let first = client.peek_session_id();
    client.end_session();
    assert_eq!(client.capture("full"), CaptureOutcome::Full);
    assert!(client.peek_session_id().is_some());
    assert_ne!(client.peek_session_id(), first);
    client.shutdown_blocking(None);

    let directory = Directory::new();
    let server = MockServer::start();
    let mut reject = server.mock(|when, then| {
        when.method(POST);
        then.status(503);
    });
    let configure = || {
        let mut conf = config();
        conf.host = server.base_url();
        conf.persistence = Persistence::Directory(directory.0.clone());
        conf.shutdown_timeout = Duration::from_millis(100);
        conf
    };
    let client = Client::init_blocking(configure());
    client.flush_blocking();
    let old = client.session_id().unwrap();
    assert!(matches!(
        client.capture("retained"),
        CaptureOutcome::Accepted { .. }
    ));
    client.flush_blocking();
    assert!(reject.calls() >= 1);
    client.reset();
    client.session_activity();
    assert_ne!(client.peek_session_id(), Some(old));
    client.shutdown_blocking(None);
    drop(client);
    reject.delete();
    let accept = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .body_includes(old.to_string())
            .body_includes("retained");
        then.status(200);
    });
    let mut conf = configure();
    conf.before_send(|_| panic!("recovery must not rerun hooks"));
    let client = Client::init_blocking(conf);
    client.flush_blocking();
    assert_eq!(client.peek_session_id(), None);
    client.shutdown_blocking(None);
    accept.assert_calls(1);
}

#[test]
fn hook_can_replace_or_remove_wire_session_without_changing_manager() {
    let server = MockServer::start();
    let mut conf = config();
    conf.host = server.base_url();
    conf.before_send(|mut event| {
        if event.event_name() == "replace" {
            event.insert_prop("$session_id", "hook-session").unwrap();
        } else {
            event.remove_prop("$session_id");
        }
        Some(event)
    });
    let client = Client::init_blocking(conf);
    let id = client.session_id();
    let wire = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .body_includes("hook-session")
            .body_excludes(id.unwrap().to_string());
        then.status(200);
    });
    client.capture("replace");
    client.capture("remove");
    client.flush_blocking();
    wire.assert_calls(1);
    assert_eq!(client.peek_session_id(), id);
}

#[cfg(feature = "async-client")]
#[test]
fn async_initialization_and_drains_share_session_persistence_without_runtime() {
    futures::executor::block_on(async {
        let directory = Directory::new();
        let client = Client::init(directory.config()).await;
        let id = client.session_id();
        assert_eq!(client.flush().await.state, PersistenceOutcome::Persisted);
        client.shutdown(None).await;
        drop(client);
        let client = Client::init(directory.config()).await;
        assert_eq!(client.session_id(), id);
        client.shutdown(None).await;
    });
}

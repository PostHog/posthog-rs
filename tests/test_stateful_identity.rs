#![cfg(feature = "stateful")]

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::Duration;

use httpmock::{Method::POST, MockServer};
use posthog::stateful::{
    Capture, CaptureOutcome, Client, Config, Identify, Persistence, PersistenceOutcome,
    PersonProfiles,
};
use posthog::Event;
use serde_json::{json, Value};
use uuid::Uuid;

fn config() -> Config {
    let mut config = Config::new("phc_test");
    config.persistence = Persistence::Memory;
    config.host = "http://127.0.0.1:1".into();
    config.bootstrap_anonymous_id = Some("anonymous".into());
    config.flush_at = 1000;
    config.flush_interval = Duration::from_secs(3600);
    config.capture_compression = None;
    config.shutdown_timeout = Duration::ZERO;
    config.max_capture_attempts = 1;
    config
}

fn observed(mut config: Config) -> (Client, Arc<Mutex<Vec<Event>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    config.before_send(move |event| {
        seen.lock().unwrap().push(event.clone());
        Some(event)
    });
    (Client::init_blocking(config), events)
}

fn properties(value: Value) -> HashMap<String, Value> {
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

#[test]
fn identity_table_and_reset_preserve_device_until_reset() {
    let (client, events) = observed(config());
    assert!(!client.is_identified());
    client.identify("  ");
    assert_eq!(client.get_distinct_id(), "anonymous");
    assert!(events.lock().unwrap().is_empty());
    client.identify("A");
    assert!(client.is_identified());
    assert_eq!(client.get_distinct_id(), "A");
    assert_eq!(client.get_anonymous_id(), "anonymous");
    client.identify("A");
    let mut other = Identify::new("B");
    other.set = properties(json!({"ignored":true}));
    client.identify(other);
    assert_eq!(client.get_distinct_id(), "A");
    let mut same = Identify::new("A");
    same.set = properties(json!({"name":"Ada"}));
    same.set_once = properties(json!({"first":null}));
    client.identify(same.clone());
    client.identify(same);
    let seen = events.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].event_name(), "$identify");
    assert_eq!(seen[0].distinct_id(), "A");
    assert_eq!(seen[0].properties()["$anon_distinct_id"], "anonymous");
    assert_eq!(seen[0].properties()["$is_identified"], true);
    assert_eq!(seen[0].options()["process_person_profile"], true);
    assert_eq!(seen[1].event_name(), "$set");
    assert_eq!(seen[1].properties()["$set_once"], json!({"first":null}));
    drop(seen);
    client.reset();
    let fresh = client.get_distinct_id();
    assert_eq!(fresh, client.get_anonymous_id());
    assert_ne!(fresh, "anonymous");
    assert!(!client.is_identified());
    assert_eq!(events.lock().unwrap().len(), 2);
    client.identify("B");
    assert_eq!(client.get_distinct_id(), "B");
    assert_eq!(
        events.lock().unwrap()[2].properties()["$anon_distinct_id"],
        fresh
    );
}

#[test]
fn same_anonymous_id_promotes_once_even_if_properties_were_cached() {
    let (client, events) = observed(config());
    let set = properties(json!({"name":"Ada"}));
    client.set_person_properties(set.clone(), HashMap::new());
    let mut input = Identify::new("anonymous");
    input.set = set;
    client.identify(input.clone());
    client.identify(input);
    assert!(client.is_identified());
    let seen = events.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().all(|event| event.event_name() == "$set"));
    assert_eq!(seen[0].properties()["$is_identified"], false);
    assert_eq!(seen[1].properties()["$is_identified"], true);
}

#[test]
fn profile_modes_and_explicit_capture_options_keep_rust_precedence() {
    for mode in [
        PersonProfiles::Never,
        PersonProfiles::IdentifiedOnly,
        PersonProfiles::Always,
    ] {
        let mut conf = config();
        conf.person_profiles = mode;
        let (client, events) = observed(conf);
        client.capture("before");
        assert_eq!(
            events.lock().unwrap()[0].options()["process_person_profile"],
            mode == PersonProfiles::Always
        );
        client.create_person_profile();
        client.create_person_profile();
        client.alias("alias");
        assert_eq!(client.get_distinct_id(), "anonymous");
        assert!(!client.is_identified());
        client.set_person_properties(properties(json!({"a":1})), HashMap::new());
        client.identify("A");
        client.capture("after");
        let seen = events.lock().unwrap();
        assert_eq!(
            seen.last().unwrap().options()["process_person_profile"],
            mode != PersonProfiles::Never
        );
        assert_eq!(
            seen.iter()
                .filter(|event| event.event_name() == "$create_alias")
                .count(),
            usize::from(mode != PersonProfiles::Never)
        );
        assert_eq!(
            seen.iter()
                .filter(|event| event.event_name() == "$set")
                .count(),
            match mode {
                PersonProfiles::Never => 0,
                PersonProfiles::IdentifiedOnly => 2,
                _ => 1,
            }
        );
        drop(seen);
        assert_eq!(client.is_identified(), mode != PersonProfiles::Never);
        let mut explicit = Capture::new("override");
        explicit
            .properties
            .insert("$process_person_profile".into(), json!(true));
        explicit
            .options
            .insert("process_person_profile".into(), json!(false));
        client.capture(explicit);
        assert_eq!(
            events.lock().unwrap().last().unwrap().options()["process_person_profile"],
            false
        );
        client.reset();
        client.capture("reset");
        assert_eq!(
            events.lock().unwrap().last().unwrap().options()["process_person_profile"],
            mode == PersonProfiles::Always
        );
    }
}

#[test]
fn rejected_identify_keeps_identity_without_replaying_link_and_properties_remain_retryable() {
    let mut conf = config();
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let seen = attempts.clone();
    conf.before_send(move |event| {
        let mut seen = seen.lock().unwrap();
        seen.push(event.event_name().to_owned());
        if seen.len() <= 2 {
            None
        } else {
            Some(event)
        }
    });
    let client = Client::init_blocking(conf);
    let mut input = Identify::new("A");
    input.set = properties(json!({"name":"Ada"}));
    client.identify(input.clone());
    assert_eq!(client.get_distinct_id(), "A");
    assert!(client.is_identified());
    client.identify("A");
    client.identify(input.clone());
    client.identify(input.clone());
    client.identify(input);
    assert_eq!(*attempts.lock().unwrap(), ["$identify", "$set", "$set"]);
}

#[test]
fn a_full_queue_keeps_identity_and_does_not_cache_a_rejected_property_update() {
    let server = MockServer::start();
    let request = server.mock(|when, then| {
        when.method(POST);
        then.status(200);
    });
    let mut conf = config();
    conf.host = server.base_url();
    conf.max_queue_size = 1;
    let (client, events) = observed(conf);
    assert!(matches!(
        client.capture("occupy"),
        CaptureOutcome::Accepted { .. }
    ));
    let mut input = Identify::new("A");
    input.set = properties(json!({"name":"Ada"}));
    client.identify(input.clone());
    assert_eq!(client.get_distinct_id(), "A");
    client.flush_blocking();
    client.identify(input.clone());
    client.identify(input);
    client.flush_blocking();
    request.assert_calls(2);
    let seen = events.lock().unwrap();
    assert_eq!(
        seen.iter().map(Event::event_name).collect::<Vec<_>>(),
        ["occupy", "$identify", "$set"]
    );
}

#[test]
fn sorted_nested_properties_deduplicate_but_array_order_and_null_do_not() {
    let (client, events) = observed(config());
    client.set_person_properties(HashMap::new(), HashMap::new());
    client.set_person_properties(
        properties(json!({"nested":{"b":2,"a":1},"arr":[null,1]})),
        HashMap::new(),
    );
    client.set_person_properties(
        properties(json!({"arr":[null,1],"nested":{"a":1,"b":2}})),
        HashMap::new(),
    );
    client.set_person_properties(
        properties(json!({"nested":{"a":1,"b":2},"arr":[1,null]})),
        HashMap::new(),
    );
    client.set_person_properties(
        properties(json!({"nested":{"a":1,"b":2},"arr":[1]})),
        HashMap::new(),
    );
    assert_eq!(events.lock().unwrap().len(), 3);
}

#[test]
fn reset_in_hook_keeps_outer_snapshot_and_reentrant_identify_cannot_mutate() {
    let holder = Arc::new(Mutex::new(Weak::<Client>::new()));
    let hook_holder = holder.clone();
    let mut conf = config();
    conf.before_send(move |event| {
        let client = hook_holder.lock().unwrap().upgrade().unwrap();
        assert!(client.is_identified());
        assert_eq!(client.get_distinct_id(), "A");
        client.identify("B");
        assert_eq!(client.get_distinct_id(), "A");
        client.reset();
        assert_eq!(event.distinct_id(), "A");
        Some(event)
    });
    let client = Arc::new(Client::init_blocking(conf));
    *holder.lock().unwrap() = Arc::downgrade(&client);
    client.identify("A");
    assert!(!client.is_identified());
    assert_ne!(client.get_distinct_id(), "A");
}

#[test]
fn slow_hook_does_not_block_reset_or_poison_new_generation_dedup() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut conf = config();
    conf.before_send(move |event| {
        if event.event_name() == "$identify" {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        Some(event)
    });
    let (client, events) = observed(conf);
    let client = Arc::new(client);
    let worker_client = client.clone();
    let set = properties(json!({"name":"Ada"}));
    let worker_set = set.clone();
    let thread = std::thread::spawn(move || {
        let mut input = Identify::new("A");
        input.set = worker_set;
        worker_client.identify(input);
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    client.reset();
    let reset = client.get_distinct_id();
    release_tx.send(()).unwrap();
    thread.join().unwrap();
    client.set_person_properties(set, HashMap::new());
    let seen = events.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].distinct_id(), "A");
    assert_eq!(seen[1].distinct_id(), reset);
    assert_eq!(seen[1].properties()["$is_identified"], false);
}

#[test]
fn invalid_state_size_disabled_and_closed_mutations_do_nothing() {
    let mut conf = config();
    conf.max_state_bytes = 256;
    let (client, events) = observed(conf);
    client.identify("x".repeat(1000));
    assert_eq!(client.get_distinct_id(), "anonymous");
    assert!(!client.is_identified());
    assert!(events.lock().unwrap().is_empty());
    client.shutdown_blocking(None);
    client.identify("A");
    client.reset();
    assert_eq!(client.get_distinct_id(), "anonymous");
    let mut conf = config();
    conf.disabled = true;
    let (client, events) = observed(conf);
    client.identify("A");
    client.alias("alias");
    client.create_person_profile();
    client.set_person_properties(properties(json!({"a":1})), HashMap::new());
    client.reset();
    assert!(!client.is_identified());
    assert!(events.lock().unwrap().is_empty());
}

struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("posthog-identity-{}", Uuid::now_v7())))
    }
    fn config(&self) -> Config {
        let mut conf = config();
        conf.persistence = Persistence::Directory(self.0.clone());
        conf.shutdown_timeout = Duration::from_secs(1);
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
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn identified_state_and_profile_enablement_survive_restart_reset_does_not_rebootstrap() {
    let directory = Directory::new();
    let mut conf = directory.config();
    conf.bootstrap_anonymous_id = None;
    conf.bootstrap_identified_id = Some("seed".into());
    let client = Client::init_blocking(conf);
    assert!(client.is_identified());
    assert_eq!(client.get_distinct_id(), "seed");
    assert_ne!(client.get_anonymous_id(), "seed");
    let device = client.get_anonymous_id();
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    drop(client);
    let client = Client::init_blocking(directory.config());
    assert!(client.is_identified());
    assert_eq!(client.get_distinct_id(), "seed");
    assert_eq!(client.get_anonymous_id(), device);
    client.reset();
    let fresh = client.get_distinct_id();
    assert_ne!(fresh, "seed");
    assert!(!client.is_identified());
    client.create_person_profile();
    client.shutdown_blocking(None);
    drop(client);
    let (client, events) = observed(directory.config());
    assert_eq!(client.get_distinct_id(), fresh);
    assert!(!client.is_identified());
    client.capture("restored-profile");
    assert_eq!(
        events.lock().unwrap()[0].options()["process_person_profile"],
        true
    );
    client.shutdown_blocking(None);
}

#[test]
fn identified_bootstrap_reconciles_anonymous_but_preserves_a_different_identified_user() {
    let directory = Directory::new();
    let client = Client::init_blocking(directory.config());
    client.shutdown_blocking(None);
    drop(client);
    let mut conf = directory.config();
    conf.bootstrap_anonymous_id = None;
    conf.bootstrap_identified_id = Some("A".into());
    let (client, events) = observed(conf);
    assert_eq!(client.get_distinct_id(), "A");
    assert_eq!(client.get_anonymous_id(), "anonymous");
    assert_eq!(events.lock().unwrap()[0].event_name(), "$identify");
    client.shutdown_blocking(None);
    drop(client);
    let mut conf = directory.config();
    conf.bootstrap_anonymous_id = None;
    conf.bootstrap_identified_id = Some("B".into());
    let (client, events) = observed(conf);
    assert_eq!(client.get_distinct_id(), "A");
    assert!(events.lock().unwrap().is_empty());
    client.shutdown_blocking(None);
}

#[test]
fn matching_bootstrap_upgrades_without_an_event_and_never_gates_reconciliation() {
    for mode in [PersonProfiles::IdentifiedOnly, PersonProfiles::Never] {
        let directory = Directory::new();
        let client = Client::init_blocking(directory.config());
        client.shutdown_blocking(None);
        drop(client);
        let mut conf = directory.config();
        conf.bootstrap_anonymous_id = None;
        conf.bootstrap_identified_id = Some("anonymous".into());
        conf.person_profiles = mode;
        let (client, events) = observed(conf);
        assert_eq!(client.is_identified(), mode != PersonProfiles::Never);
        assert_eq!(client.get_anonymous_id(), "anonymous");
        assert!(events.lock().unwrap().is_empty());
        client.shutdown_blocking(None);
    }
    let mut fresh_never = config();
    fresh_never.bootstrap_anonymous_id = None;
    fresh_never.bootstrap_identified_id = Some("seed".into());
    fresh_never.person_profiles = PersonProfiles::Never;
    let (client, events) = observed(fresh_never);
    assert!(client.is_identified());
    client.capture("seeded");
    assert_eq!(
        events.lock().unwrap()[0].options()["process_person_profile"],
        false
    );
    let mut explicit = Capture::new("override");
    explicit
        .options
        .insert("process_person_profile".into(), json!(true));
    client.capture(explicit);
    assert_eq!(
        events.lock().unwrap()[1].options()["process_person_profile"],
        true
    );
    let mut conf = config();
    conf.bootstrap_identified_id = Some("conflict".into());
    assert!(Client::init_blocking(conf).is_disabled());
}

#[test]
fn version_one_state_migrates_on_mutation_and_future_state_is_preserved() {
    let directory = Directory::new();
    let client = Client::init_blocking(directory.config());
    client.shutdown_blocking(None);
    drop(client);
    let path = directory.state_path();
    std::fs::write(
        &path,
        br#"{"version":1,"state":{"anonymous_id":"legacy","distinct_id":"legacy"}}"#,
    )
    .unwrap();
    let client = Client::init_blocking(directory.config());
    assert_eq!(client.get_distinct_id(), "legacy");
    assert!(!client.is_identified());
    client.identify("A");
    client.shutdown_blocking(None);
    drop(client);
    let state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(state["version"], 3);
    assert_eq!(state["state"]["identified"], true);
    assert_eq!(state["state"]["anonymous_id"], "legacy");
    assert!(state["state"].get("person_properties").is_none());
    let bytes = br#"{"version":99,"state":{}}"#;
    std::fs::write(&path, bytes).unwrap();
    let client = Client::init_blocking(directory.config());
    assert!(!client.is_disabled());
    client.identify("B");
    client.shutdown_blocking(None);
    drop(client);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn state_write_failure_preserves_live_identity_and_retries_the_latest_snapshot() {
    let directory = Directory::new();
    let client = Client::init_blocking(directory.config());
    client.shutdown_blocking(None);
    drop(client);
    let path = directory.state_path();
    let temporary = path.with_file_name("state.json.tmp");
    std::fs::create_dir(&temporary).unwrap();
    let mut conf = directory.config();
    // Keep state persistence recovery independent of HTTP delivery.
    conf.before_send(|_| None);
    let client = Client::init_blocking(conf);
    client.identify("A");
    assert_eq!(
        client.flush_blocking().state,
        PersistenceOutcome::WriteFailed
    );
    assert_eq!(client.get_distinct_id(), "A");
    assert!(client.is_identified());
    std::fs::remove_dir(&temporary).unwrap();
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    drop(client);
    let client = Client::init_blocking(directory.config());
    assert_eq!(client.get_distinct_id(), "A");
    assert!(client.is_identified());
    client.shutdown_blocking(None);
}

#[test]
fn wire_events_retain_pre_reset_identity_and_person_payloads() {
    let server = MockServer::start();
    let delivered = server.mock(|when, then| {
        when.method(POST)
            .body_includes("\"event\":\"$identify\"")
            .body_includes("\"$anon_distinct_id\":\"anonymous\"")
            .body_includes("\"event\":\"$create_alias\"")
            .body_includes("\"alias\":\"other\"")
            .body_includes("\"distinct_id\":\"A\"")
            .body_includes("\"$set\":{\"name\":\"Ada\"}")
            .body_includes("\"$set_once\":{\"first\":null}")
            .body_includes("\"process_person_profile\":true");
        then.status(200);
    });
    let mut conf = config();
    conf.host = server.base_url();
    let client = Client::init_blocking(conf);
    let mut input = Identify::new("A");
    input.set = properties(json!({"name":"Ada"}));
    input.set_once = properties(json!({"first":null}));
    client.identify(input);
    client.alias("other");
    client.reset();
    client.flush_blocking();
    delivered.assert_calls(1);
}

#[cfg(feature = "async-client")]
#[test]
fn async_initialization_exposes_the_same_synchronous_identity_api() {
    futures::executor::block_on(async {
        let client = Client::init(config()).await;
        client.identify("A");
        assert!(client.is_identified());
        assert_eq!(client.get_distinct_id(), "A");
        client.reset();
        assert!(!client.is_identified());
        client.shutdown(Some(Duration::ZERO)).await;
    });
}

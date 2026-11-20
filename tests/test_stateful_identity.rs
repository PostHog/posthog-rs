#![cfg(feature = "stateful")]

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::Duration;

use httpmock::{Method::POST, MockServer};
use posthog::stateful::{
    Alias, Capture, Client, Config, DeliveryOutcome, DrainReport, Identify, Persistence,
    PersistenceOutcome, PersonProfiles, PersonProperties,
};
use posthog::Event;
use serde_json::{json, Value};
use uuid::Uuid;

fn config() -> Config {
    let mut config = Config::new("phc_test");
    config.preload_feature_flags = false;
    config.persistence = Persistence::Memory;
    config.host = "http://127.0.0.1:1".into();
    config.bootstrap_anonymous_id = Some("anonymous".into());
    config.flush_at = 1000;
    config.flush_interval = Duration::from_secs(3600);
    config.capture_compression = None;
    config.shutdown_timeout = Duration::ZERO;
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

/// Captures a probe event and returns the `$is_identified` value it was sent with.
fn probe_identified(client: &Client, events: &Mutex<Vec<Event>>) -> bool {
    client.capture("identity probe");
    let seen = events.lock().unwrap();
    let probe = seen.last().unwrap();
    assert_eq!(probe.event_name(), "identity probe");
    probe.properties()["$is_identified"].as_bool().unwrap()
}

#[test]
fn identity_table_and_reset_preserve_device_across_reset() {
    let (client, events) = observed(config());
    client.identify("  ");
    assert_eq!(client.get_distinct_id(), "anonymous");
    assert!(events.lock().unwrap().is_empty());
    client.identify("A");
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
    assert_eq!(events.lock().unwrap().len(), 2);
    client.identify("B");
    assert_eq!(client.get_distinct_id(), "B");
    assert_eq!(events.lock().unwrap()[2].event_name(), "$identify");
    assert_eq!(
        events.lock().unwrap()[2].properties()["$anon_distinct_id"],
        fresh
    );
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .all(|event| event.properties()["$device_id"] == "anonymous"));
    client.reset();
    assert_ne!(client.get_anonymous_id(), fresh);
    client.capture("second-reset");
    assert_eq!(
        events.lock().unwrap().last().unwrap().properties()["$device_id"],
        "anonymous"
    );
    assert_eq!(
        events.lock().unwrap().last().unwrap().properties()["$is_identified"],
        false
    );
}

#[test]
fn same_anonymous_id_promotes_once_even_if_properties_were_cached() {
    let (client, events) = observed(config());
    let set = properties(json!({"name":"Ada"}));
    client.set_person_properties(set.clone());
    let mut input = Identify::new("anonymous");
    input.set = set;
    client.identify(input.clone());
    client.identify(input);
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
        // The probe is not a person event, so the counts below are unchanged.
        assert!(!probe_identified(&client, &events));
        client.set_person_properties(properties(json!({"a":1})));
        client.identify("A");
        client.capture("after");
        let seen = events.lock().unwrap();
        assert_eq!(
            seen.last().unwrap().options()["process_person_profile"],
            mode != PersonProfiles::Never
        );
        assert_eq!(
            seen.last().unwrap().properties()["$is_identified"],
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
    client.identify("A");
    // A repeat with no properties sends nothing only while still identified.
    assert_eq!(attempts.lock().unwrap().len(), 1);
    client.identify(input.clone());
    client.identify(input.clone());
    client.identify(input);
    assert_eq!(*attempts.lock().unwrap(), ["$identify", "$set", "$set"]);
}

#[test]
fn sorted_nested_properties_deduplicate_but_array_order_and_null_do_not() {
    let (client, events) = observed(config());
    client.set_person_properties(HashMap::new());
    client.set_person_properties(properties(json!({"nested":{"b":2,"a":1},"arr":[null,1]})));
    client.set_person_properties(properties(json!({"arr":[null,1],"nested":{"a":1,"b":2}})));
    client.set_person_properties(properties(json!({"nested":{"a":1,"b":2},"arr":[1,null]})));
    client.set_person_properties(properties(json!({"nested":{"a":1,"b":2},"arr":[1]})));
    assert_eq!(events.lock().unwrap().len(), 3);
}

#[test]
fn reset_in_hook_keeps_outer_snapshot_and_reentrant_identify_cannot_mutate() {
    let holder = Arc::new(Mutex::new(Weak::<Client>::new()));
    let hook_holder = holder.clone();
    let mut conf = config();
    conf.before_send(move |event| {
        if event.event_name() != "$identify" {
            return Some(event);
        }
        let client = hook_holder.lock().unwrap().upgrade().unwrap();
        assert_eq!(event.properties()["$is_identified"], true);
        assert_eq!(client.get_distinct_id(), "A");
        client.identify("B");
        assert_eq!(client.get_distinct_id(), "A");
        client.reset();
        assert_eq!(event.distinct_id(), "A");
        Some(event)
    });
    let (client, events) = observed(conf);
    let client = Arc::new(client);
    *holder.lock().unwrap() = Arc::downgrade(&client);
    client.identify("A");
    assert!(!probe_identified(&client, &events));
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
    client.set_person_properties(set);
    let seen = events.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].distinct_id(), "A");
    assert_eq!(seen[1].distinct_id(), reset);
    assert_eq!(seen[1].properties()["$is_identified"], false);
}

#[derive(Clone)]
struct Recorder(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Recorder {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Keeps a second dispatch alive for the whole test process. With a single
/// dispatch, tracing-core registers a new callsite against the registering
/// thread's default, so a thread without a recorder could hide a log line from
/// a thread that has one.
fn keep_callsites_shared() {
    static REGISTRATION: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    REGISTRATION.get_or_init(|| tracing::Dispatch::new(tracing_subscriber::registry()));
}

/// Records this thread's log output at `level` and above while the guard lives.
fn record_logs(level: tracing::Level) -> (Arc<Mutex<Vec<u8>>>, tracing::subscriber::DefaultGuard) {
    keep_callsites_shared();
    let output = Arc::new(Mutex::new(Vec::new()));
    let sink = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(level)
        .with_writer(move || Recorder(sink.clone()))
        .finish();
    (output, tracing::subscriber::set_default(subscriber))
}

#[test]
fn rejected_identity_updates_report_sanitized_reasons() {
    let (client, events) = observed(config());
    let original = client.get_distinct_id();
    let (output, _guard) = record_logs(tracing::Level::WARN);
    client.identify(" \t");
    assert_eq!(client.get_distinct_id(), original);
    assert!(events.lock().unwrap().is_empty());
    assert!(!probe_identified(&client, &events));
    let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("identity update requires a nonblank distinct ID"));
    output.lock().unwrap().clear();
    client.shutdown_blocking(None);
    client.identify(" \t");
    assert!(output.lock().unwrap().is_empty());
}

#[test]
fn ignored_identity_updates_are_logged_without_values() {
    let mut conf = config();
    conf.person_profiles = PersonProfiles::Never;
    let (client, events) = observed(conf);
    let (output, guard) = record_logs(tracing::Level::WARN);
    let mut input = Identify::new("private-user");
    input.set = properties(json!({"private-key":"private-value"}));
    client.identify(input);
    client.alias("private-alias");
    client.set_person_properties(properties(json!({"private-key":"private-value"})));
    client.create_person_profile();
    assert!(events.lock().unwrap().is_empty());
    let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();
    for operation in [
        "identify",
        "alias",
        "set_person_properties",
        "create_person_profile",
    ] {
        let message = format!("{operation} is ignored because person profiles are disabled");
        assert!(logs.contains(&message), "missing log: {}", message);
    }
    assert!(!logs.contains("private"));
    drop(guard);

    let (client, events) = observed(config());
    let (output, _guard) = record_logs(tracing::Level::DEBUG);
    client.identify("private-user");
    output.lock().unwrap().clear();
    client.identify("private-user");
    assert_eq!(events.lock().unwrap().len(), 1);
    let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("already identified with this distinct ID; nothing to send"));
    assert!(!logs.contains("private"));
}

#[test]
fn disabled_and_closed_mutations_do_nothing() {
    let (client, _) = observed(config());
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
    client.set_person_properties(properties(json!({"a":1})));
    client.reset();
    assert!(events.lock().unwrap().is_empty());
}

struct Directory {
    path: std::path::PathBuf,
    // Accepts deliveries so shutdown can drain instead of retrying a refused host.
    server: MockServer,
}
impl Directory {
    fn new() -> Self {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST);
            then.status(200);
        });
        Self {
            path: std::env::temp_dir().join(format!("posthog-identity-{}", Uuid::now_v7())),
            server,
        }
    }
    fn config(&self) -> Config {
        let mut conf = config();
        conf.persistence = Persistence::Directory(self.path.clone());
        conf.host = self.server.base_url();
        // Shutdown returns once the workers stop; the deadline only bounds the
        // wait. A loaded runner must not expire it before storage is released.
        conf.shutdown_timeout = Duration::from_secs(30);
        conf
    }
    fn state_path(&self) -> std::path::PathBuf {
        std::fs::read_dir(&self.path)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("state.json")
    }
    /// The persisted `identified` flag. Read it only after a shutdown that persisted state.
    fn identified(&self) -> bool {
        let state: Value =
            serde_json::from_slice(&std::fs::read(self.state_path()).unwrap()).unwrap();
        state["state"]["identified"].as_bool().unwrap()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Shuts down a client whose storage is opened again. The client releases its
/// storage only after delivery stops, so a reopen after a timed-out shutdown
/// falls back to memory.
fn close_before_reopen(client: Client) -> DrainReport {
    let report = client.shutdown_blocking(None);
    assert_eq!(report.delivery, DeliveryOutcome::Stopped);
    report
}

#[cfg(feature = "async-client")]
async fn close_before_reopen_async(client: Client) -> DrainReport {
    let report = client.shutdown(None).await;
    assert_eq!(report.delivery, DeliveryOutcome::Stopped);
    report
}

#[test]
fn identified_state_and_profile_enablement_survive_restart_reset_does_not_rebootstrap() {
    let directory = Directory::new();
    let mut conf = directory.config();
    conf.bootstrap_anonymous_id = None;
    conf.bootstrap_identified_id = Some("seed".into());
    let client = Client::init_blocking(conf);
    assert_eq!(client.get_distinct_id(), "seed");
    assert_ne!(client.get_anonymous_id(), "seed");
    let device = client.get_anonymous_id();
    assert_eq!(
        close_before_reopen(client).state,
        PersistenceOutcome::Persisted
    );
    assert!(directory.identified());
    let (client, events) = observed(directory.config());
    assert!(probe_identified(&client, &events));
    assert_eq!(client.get_distinct_id(), "seed");
    assert_eq!(client.get_anonymous_id(), device);
    client.reset();
    let fresh = client.get_distinct_id();
    assert_ne!(fresh, "seed");
    client.create_person_profile();
    close_before_reopen(client);
    assert!(!directory.identified());
    let (client, events) = observed(directory.config());
    assert_eq!(client.get_distinct_id(), fresh);
    client.capture("restored-profile");
    assert_eq!(
        events.lock().unwrap()[0].properties()["$is_identified"],
        false
    );
    assert_eq!(events.lock().unwrap()[0].properties()["$device_id"], device);
    assert_eq!(
        events.lock().unwrap()[0].options()["process_person_profile"],
        true
    );
    client.shutdown_blocking(None);
}

#[cfg(feature = "async-client")]
#[tokio::test(flavor = "current_thread")]
async fn async_reset_and_restart_keep_device_separate_from_anonymous_getter() {
    let directory = Directory::new();
    let client = Client::init(directory.config()).await;
    client.identify("person");
    client.reset();
    let anonymous = client.get_anonymous_id();
    assert_ne!(anonymous, "anonymous");
    assert_eq!(
        close_before_reopen_async(client).await.state,
        PersistenceOutcome::Persisted
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let mut conf = directory.config();
    conf.before_send(move |event| {
        seen.lock().unwrap().push(event.clone());
        Some(event)
    });
    let client = Client::init(conf).await;
    assert_eq!(client.get_anonymous_id(), anonymous);
    client.capture("after-restart");
    assert_eq!(
        events.lock().unwrap()[0].properties()["$device_id"],
        "anonymous"
    );
    client.shutdown(None).await;
}

#[test]
fn identified_bootstrap_reconciles_anonymous_but_preserves_a_different_identified_user() {
    let directory = Directory::new();
    let client = Client::init_blocking(directory.config());
    close_before_reopen(client);
    let mut conf = directory.config();
    conf.bootstrap_anonymous_id = None;
    conf.bootstrap_identified_id = Some("A".into());
    let (client, events) = observed(conf);
    assert_eq!(client.get_distinct_id(), "A");
    assert_eq!(client.get_anonymous_id(), "anonymous");
    assert_eq!(events.lock().unwrap()[0].event_name(), "$identify");
    close_before_reopen(client);
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
        close_before_reopen(client);
        let mut conf = directory.config();
        conf.bootstrap_anonymous_id = None;
        conf.bootstrap_identified_id = Some("anonymous".into());
        conf.person_profiles = mode;
        let (client, events) = observed(conf);
        assert_eq!(client.get_distinct_id(), "anonymous");
        assert_eq!(client.get_anonymous_id(), "anonymous");
        assert!(events.lock().unwrap().is_empty());
        client.capture("restored seed");
        assert_eq!(
            events.lock().unwrap()[0].properties()["$is_identified"],
            true
        );
        assert_eq!(
            events.lock().unwrap()[0].options()["process_person_profile"],
            mode != PersonProfiles::Never
        );
        close_before_reopen(client);
        let mut restored = directory.config();
        restored.person_profiles = mode;
        let (client, events) = observed(restored);
        assert!(probe_identified(&client, &events));
        assert_eq!(client.get_distinct_id(), "anonymous");
        assert_eq!(client.get_anonymous_id(), "anonymous");
        client.shutdown_blocking(None);
    }
    let mut fresh_never = config();
    fresh_never.bootstrap_anonymous_id = None;
    fresh_never.bootstrap_identified_id = Some("seed".into());
    fresh_never.person_profiles = PersonProfiles::Never;
    let (client, events) = observed(fresh_never);
    client.capture("seeded");
    assert_eq!(
        events.lock().unwrap()[0].properties()["$is_identified"],
        true
    );
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
fn current_state_restores_and_future_state_is_preserved() {
    let directory = Directory::new();
    let client = Client::init_blocking(directory.config());
    assert_eq!(client.flush_blocking().state, PersistenceOutcome::Persisted);
    close_before_reopen(client);
    let path = directory.state_path();
    let client = Client::init_blocking(directory.config());
    assert_eq!(client.get_distinct_id(), "anonymous");
    client.identify("A");
    assert_eq!(client.flush_blocking().state, PersistenceOutcome::Persisted);
    close_before_reopen(client);
    let state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(state["version"], 1);
    assert_eq!(state["state"]["identified"], true);
    // Identify switched users, so the restored state was anonymous.
    assert_eq!(state["state"]["distinct_id"], "A");
    assert_eq!(state["state"]["anonymous_id"], "anonymous");
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
    let server = MockServer::start();
    let flags = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({"featureFlags": {}}));
    });
    let config = || {
        let mut conf = directory.config();
        conf.host = server.base_url();
        conf
    };
    let client = Client::init_blocking(config());
    close_before_reopen(client);
    let path = directory.state_path();
    let temporary = path.with_file_name("state.json.tmp");
    std::fs::create_dir(&temporary).unwrap();
    let mut conf = config();
    // Keep state persistence recovery independent of HTTP delivery.
    conf.before_send(|_| None);
    let (tx, rx) = std::sync::mpsc::channel();
    conf.on_feature_flags(move |_| {
        let _ = tx.send(());
    });
    let client = Client::init_blocking(conf);
    client.identify("A");
    assert_eq!(
        client.flush_blocking().state,
        PersistenceOutcome::WriteFailed
    );
    assert_eq!(client.get_distinct_id(), "A");
    rx.recv_timeout(Duration::from_secs(5))
        .expect("identity flags completion");
    flags.assert_calls(1);
    std::fs::remove_dir(&temporary).unwrap();
    assert_eq!(
        close_before_reopen(client).state,
        PersistenceOutcome::Persisted
    );
    assert!(directory.identified());
    let (client, events) = observed(config());
    assert_eq!(client.get_distinct_id(), "A");
    assert!(probe_identified(&client, &events));
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
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let mut conf = config();
        conf.before_send(move |event| {
            seen.lock().unwrap().push(event.clone());
            Some(event)
        });
        let client = Client::init(conf).await;
        client.identify("A");
        assert_eq!(
            events.lock().unwrap()[0].properties()["$is_identified"],
            true
        );
        assert_eq!(client.get_distinct_id(), "A");
        client.reset();
        assert!(!probe_identified(&client, &events));
        client.shutdown(Some(Duration::ZERO)).await;
    });
}

#[test]
fn blank_or_self_alias_sends_nothing() {
    let (client, events) = observed(config());
    client.alias(" ");
    client.alias("anonymous");
    assert!(events.lock().unwrap().is_empty());
    client.alias("other");
    let seen = events.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].event_name(), "$create_alias");
}

#[test]
fn identity_options_reach_their_events() {
    let (client, events) = observed(config());
    let mut identify = Identify::new("A");
    identify
        .options
        .insert("cookieless_mode".into(), json!(true));
    client.identify(identify);
    let mut alias = Alias::new("other");
    alias.options.insert("cookieless_mode".into(), json!(true));
    client.alias(alias);
    let mut update = PersonProperties::new();
    update.set.insert("plan".into(), json!("pro"));
    update.options.insert("cookieless_mode".into(), json!(true));
    client.set_person_properties(update);
    let seen = events.lock().unwrap();
    assert_eq!(seen.len(), 3);
    for event in seen.iter() {
        assert_eq!(
            event.options()["cookieless_mode"],
            true,
            "{}",
            event.event_name()
        );
    }
}

#[test]
fn alias_and_person_properties_each_enable_anonymous_processing() {
    for alias in [true, false] {
        let (client, events) = observed(config());
        if alias {
            client.alias("other");
        } else {
            client.set_person_properties(properties(json!({"plan": "pro"})));
        }
        client.capture("after");
        assert_eq!(client.get_distinct_id(), "anonymous");
        assert_eq!(client.get_anonymous_id(), "anonymous");
        let seen = events.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].properties()["$is_identified"], false);
        for event in seen.iter() {
            assert_eq!(
                event.options()["process_person_profile"],
                true,
                "alias={alias} {}",
                event.event_name()
            );
        }
    }
}

#[test]
fn set_once_changes_are_not_deduplicated_away() {
    let (client, events) = observed(config());
    let update = |set: Value, set_once: Value| {
        let mut update = PersonProperties::new();
        update.set = properties(set);
        update.set_once = properties(set_once);
        update
    };
    client.set_person_properties(update(json!({"a": 1}), json!({"b": 1})));
    // Only set_once changes.
    client.set_person_properties(update(json!({"a": 1}), json!({"b": 2})));
    // An exact repeat sends nothing.
    client.set_person_properties(update(json!({"a": 1}), json!({"b": 2})));
    // set_once with an empty set.
    client.set_person_properties(update(json!({}), json!({"b": 2})));
    // The same property moved from set_once to set.
    client.set_person_properties(update(json!({"b": 2}), json!({})));
    let seen = events.lock().unwrap();
    let payloads: Vec<_> = seen
        .iter()
        .map(|event| {
            (
                event.properties()["$set"].clone(),
                event.properties()["$set_once"].clone(),
            )
        })
        .collect();
    assert_eq!(
        payloads,
        vec![
            (json!({"a": 1}), json!({"b": 1})),
            (json!({"a": 1}), json!({"b": 2})),
            (json!({}), json!({"b": 2})),
            (json!({"b": 2}), json!({})),
        ]
    );
}

#[test]
fn never_mode_bootstrap_keeps_a_restored_anonymous_install() {
    let directory = Directory::new();
    let client = Client::init_blocking(directory.config());
    close_before_reopen(client);
    let mut conf = directory.config();
    conf.bootstrap_anonymous_id = None;
    conf.bootstrap_identified_id = Some("seed".into());
    conf.person_profiles = PersonProfiles::Never;
    let (client, events) = observed(conf);
    assert_eq!(client.get_distinct_id(), "anonymous");
    assert!(events.lock().unwrap().is_empty());
    client.shutdown_blocking(None);
    assert!(!directory.identified());
}

#[test]
fn reset_forgets_the_last_screen_name() {
    let (client, events) = observed(config());
    client.screen("Home");
    client.reset();
    client.capture("after reset");
    let seen = events.lock().unwrap();
    let after = seen.last().unwrap();
    assert_eq!(after.event_name(), "after reset");
    assert!(after.properties().get("$screen_name").is_none());
    drop(seen);
    client.shutdown_blocking(None);
}

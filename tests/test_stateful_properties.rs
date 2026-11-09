#![cfg(feature = "stateful")]

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::Duration;

use httpmock::{Method::POST, MockServer};
use posthog::stateful::{
    Capture, CaptureOutcome, Client, Config, Persistence, PersistenceOutcome, PersonProfiles,
};
use posthog::Event;
use serde_json::{json, Value};
use uuid::Uuid;

fn properties(value: Value) -> HashMap<String, Value> {
    serde_json::from_value(value).unwrap()
}

fn config() -> Config {
    let mut conf = Config::new("phc_test");
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
        seen.lock().unwrap().push(event);
        None
    });
    (Client::init_blocking(conf), events)
}

struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("posthog-properties-{}", Uuid::now_v7())))
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
fn registration_owns_values_replaces_top_level_and_unregister_is_local() {
    let (client, events) = observed(config());
    let mut input = properties(json!({"plan":"old", "object":{"old":1}, "array":[1,null]}));
    client.register(input.clone());
    input.insert("array".into(), json!([2]));
    client.register(properties(json!({"plan":null, "object":{"new":2}})));
    assert!(events.lock().unwrap().is_empty());
    client.capture("first");
    client.unregister("plan");
    client.unregister("missing");
    client.capture("second");
    let seen = events.lock().unwrap();
    assert_eq!(seen[0].properties()["plan"], Value::Null);
    assert_eq!(seen[0].properties()["object"], json!({"new":2}));
    assert_eq!(seen[0].properties()["array"], json!([1, null]));
    assert!(!seen[1].properties().contains_key("plan"));
    assert_eq!(seen.len(), 2);
}

#[test]
fn direct_group_identify_enables_processing_without_membership_or_identification() {
    let (client, events) = observed(config());
    client.group_identify("company", "a", HashMap::new());
    client.group_identify("company", "a", HashMap::new());
    client.capture("after");
    assert!(!client.is_identified());
    let seen = events.lock().unwrap();
    assert_eq!(seen.len(), 3);
    for event in seen.iter() {
        assert_eq!(event.options()["process_person_profile"], true);
        assert!(!event.properties().contains_key("$groups"));
    }
}

#[test]
fn concurrent_registrations_are_observed_as_whole_snapshots() {
    let (client, events) = observed(config());
    let client = Arc::new(client);
    client.register(properties(json!({"left":0,"right":0})));
    let mut writers = Vec::new();
    for lane in 0..4 {
        let client = client.clone();
        writers.push(std::thread::spawn(move || {
            for i in 0..50 {
                let value = lane * 50 + i;
                client.register(properties(json!({"left":value,"right":value})));
                client.capture("snapshot");
            }
        }));
    }
    for writer in writers {
        writer.join().unwrap();
    }
    let seen = events.lock().unwrap();
    assert_eq!(seen.len(), 200);
    for event in seen.iter() {
        assert_eq!(event.properties()["left"], event.properties()["right"]);
    }
}

#[test]
fn groups_emit_every_call_and_direct_identify_does_not_replace_membership() {
    let (client, events) = observed(config());
    client.register(properties(json!({"plan":"pro"})));
    client.group("company", "a", None);
    client.group("company", "a", None);
    client.group(
        "company",
        "a",
        Some(properties(json!({"name":"A", "clear":null}))),
    );
    client.group_identify("company", "b", properties(json!({"$group_key":"literal"})));
    client.group("team", "t", None);
    client.capture("before-remove");
    client.remove_group("company");
    client.remove_group("missing");
    client.capture("after-remove");
    client.group("team", "new", None);
    client.capture("replacement");
    assert!(!client.is_identified());
    let seen = events.lock().unwrap();
    assert_eq!(seen.len(), 9);
    for event in &seen[..5] {
        assert_eq!(event.event_name(), "$groupidentify");
        assert_eq!(event.distinct_id(), "anonymous");
        assert_eq!(event.options()["process_person_profile"], true);
        assert_eq!(event.properties()["plan"], "pro");
    }
    assert_eq!(seen[0].properties()["$group_set"], json!({}));
    assert_eq!(
        seen[2].properties()["$group_set"],
        json!({"name":"A", "clear":null})
    );
    assert_eq!(seen[3].properties()["$group_key"], "b");
    assert_eq!(
        seen[3].properties()["$group_set"],
        json!({"$group_key":"literal"})
    );
    assert_eq!(
        seen[5].properties()["$groups"],
        json!({"company":"a", "team":"t"})
    );
    assert_eq!(seen[6].properties()["$groups"], json!({"team":"t"}));
    assert_eq!(seen[6].options()["process_person_profile"], true);
    assert_eq!(seen[8].properties()["$groups"], json!({"team":"new"}));
}

#[test]
fn processing_and_consent_matrix_preserves_local_registration_and_per_event_semantics() {
    for mode in [
        PersonProfiles::Never,
        PersonProfiles::IdentifiedOnly,
        PersonProfiles::Always,
    ] {
        for opted_out in [false, true] {
            let mut conf = config();
            conf.person_profiles = mode;
            conf.opt_out = opted_out;
            let (client, events) = observed(conf);
            client.register(properties(json!({"keep":1, "remove":2})));
            client.unregister("remove");
            client.group("company", "a", None);
            client.group_identify("company", "b", HashMap::new());
            let allowed = !opted_out && mode != PersonProfiles::Never;
            assert_eq!(events.lock().unwrap().len(), if allowed { 2 } else { 0 });
            client.opt_in();
            let mut input = Capture::new("capture");
            input.groups.insert("event-only".into(), "e".into());
            client.capture(input);
            client.capture("later");
            let seen = events.lock().unwrap();
            let capture = &seen[seen.len() - 2];
            let later = seen.last().unwrap();
            assert_eq!(capture.properties()["keep"], 1);
            assert!(!capture.properties().contains_key("remove"));
            assert_eq!(capture.properties()["$groups"]["event-only"], "e");
            assert_eq!(
                later.properties().get("$groups").cloned(),
                allowed.then(|| json!({"company":"a"}))
            );
            assert_eq!(
                later.options()["process_person_profile"],
                allowed || mode == PersonProfiles::Always
            );
        }
    }
}

#[test]
fn removal_while_out_or_never_and_reset_clear_context_without_events() {
    let directory = Directory::new();
    let (client, _) = observed(directory.config());
    client.register(properties(json!({"plan":"pro"})));
    client.group("company", "a", None);
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    drop(client);
    let mut conf = directory.config();
    conf.person_profiles = PersonProfiles::Never;
    conf.opt_out = true;
    let (client, events) = observed(conf);
    client.remove_group("company");
    client.unregister("plan");
    assert_eq!(client.flush_blocking().state, PersistenceOutcome::Persisted);
    assert_eq!(directory.state()["state"]["groups"], json!({}));
    assert_eq!(directory.state()["state"]["registered"], json!({}));
    assert_eq!(directory.state()["state"]["person_processing"], true);
    client.register(properties(json!({"new":1})));
    client.reset();
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    assert_eq!(directory.state()["state"]["registered"], json!({}));
    assert_eq!(directory.state()["state"]["groups"], json!({}));
    assert_eq!(directory.state()["state"]["person_processing"], false);
    assert!(events.lock().unwrap().is_empty());
}

#[test]
fn invalid_groups_and_oversized_context_reject_whole_candidate() {
    let mut conf = config();
    conf.max_state_bytes = 512;
    let (client, events) = observed(conf);
    assert!(!client.is_disabled());
    client.register(properties(json!({"plan":"old"})));
    client.register(properties(
        json!({"plan":"changed", "large":"x".repeat(512)}),
    ));
    for (kind, key) in [(" ", "a"), ("company", "\t")] {
        client.group(kind, key, None);
        client.group_identify(kind, key, HashMap::new());
    }
    client.group("company", "x".repeat(512), None);
    assert!(events.lock().unwrap().is_empty());
    client.capture("after");
    let seen = events.lock().unwrap();
    assert_eq!(seen[0].properties()["plan"], "old");
    assert!(!seen[0].properties().contains_key("large"));
    assert!(!seen[0].properties().contains_key("$groups"));
    assert_eq!(seen[0].options()["process_person_profile"], false);
}

#[test]
fn disabled_and_closed_operations_do_not_mutate_persisted_context() {
    for disabled in [true, false] {
        let directory = Directory::new();
        let (seed, _) = observed(directory.config());
        seed.register(properties(json!({"plan":"old"})));
        seed.group("company", "old", None);
        seed.shutdown_blocking(None);
        drop(seed);
        let bytes = std::fs::read(directory.state_path()).unwrap();
        let mut conf = directory.config();
        conf.disabled = disabled;
        let (client, events) = observed(conf);
        if !disabled {
            client.shutdown_blocking(None);
        }
        client.register(properties(json!({"plan":"new"})));
        client.unregister("plan");
        client.group("company", "new", None);
        client.group_identify("company", "new", HashMap::new());
        client.remove_group("company");
        drop(client);
        assert!(events.lock().unwrap().is_empty());
        assert_eq!(std::fs::read(directory.state_path()).unwrap(), bytes);
    }
}

#[test]
fn restart_restores_owned_context_and_removals() {
    let directory = Directory::new();
    let (client, _) = observed(directory.config());
    client.register(properties(
        json!({"object":{"nested":null}, "list":[1,null]}),
    ));
    client.group("company", "a", None);
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    drop(client);
    let (client, events) = observed(directory.config());
    client.capture("restored");
    assert_eq!(
        events.lock().unwrap()[0].properties()["$groups"],
        json!({"company":"a"})
    );
    assert_eq!(
        events.lock().unwrap()[0].properties()["object"],
        json!({"nested":null})
    );
    client.remove_group("company");
    client.unregister("object");
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    drop(client);
    let (client, events) = observed(directory.config());
    client.capture("removed");
    let seen = events.lock().unwrap();
    assert!(!seen[0].properties().contains_key("$groups"));
    assert!(!seen[0].properties().contains_key("object"));
    assert_eq!(seen[0].properties()["list"], json!([1, null]));
}

#[test]
fn legacy_context_migrates_on_mutation_and_invalid_current_records_are_preserved() {
    for version in [1, 2, 3] {
        let directory = Directory::new();
        let client = Client::init_blocking(directory.config());
        client.shutdown_blocking(None);
        drop(client);
        let mut state = json!({"anonymous_id":"legacy", "distinct_id":"legacy"});
        if version >= 2 {
            state["identified"] = json!(true);
            state["person_processing"] = json!(true);
        }
        if version == 3 {
            state["opt_out"] = json!(false);
        }
        let bytes = serde_json::to_vec(&json!({"version":version, "state":state})).unwrap();
        std::fs::write(directory.state_path(), &bytes).unwrap();
        let (client, events) = observed(directory.config());
        assert_eq!(client.get_distinct_id(), "legacy");
        client.capture("old");
        assert!(!events.lock().unwrap()[0]
            .properties()
            .contains_key("$groups"));
        assert_eq!(std::fs::read(directory.state_path()).unwrap(), bytes);
        client.register(properties(json!({"migrated":true})));
        assert_eq!(
            client.shutdown_blocking(None).state,
            PersistenceOutcome::Persisted
        );
        assert_eq!(directory.state()["version"], 4);
        assert_eq!(
            directory.state()["state"]["registered"],
            json!({"migrated":true})
        );
        assert_eq!(directory.state()["state"]["groups"], json!({}));
    }
    for invalid in [json!(null), json!({"company":1}), json!({" ":"key"})] {
        let directory = Directory::new();
        let client = Client::init_blocking(directory.config());
        client.shutdown_blocking(None);
        drop(client);
        let mut state = directory.state();
        state["state"]["groups"] = invalid;
        let bytes = serde_json::to_vec(&state).unwrap();
        std::fs::write(directory.state_path(), &bytes).unwrap();
        let (client, _) = observed(directory.config());
        assert!(!client.is_disabled());
        client.register(properties(json!({"fallback":true})));
        assert_eq!(
            client.shutdown_blocking(None).state,
            PersistenceOutcome::Unavailable
        );
        assert_eq!(std::fs::read(directory.state_path()).unwrap(), bytes);
    }
}

#[test]
fn dropped_or_full_group_event_keeps_context_without_replay() {
    let server = MockServer::start();
    let request = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .body_includes("\"event\":\"occupy\"");
        then.status(200);
    });
    let mut conf = config();
    conf.host = server.base_url();
    conf.max_queue_size = 1;
    conf.flush_at = 1000;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let events = seen.clone();
    conf.before_send(move |event| {
        events.lock().unwrap().push(event.clone());
        Some(event)
    });
    let client = Client::init_blocking(conf);
    assert!(matches!(
        client.capture("occupy"),
        CaptureOutcome::Accepted { .. }
    ));
    client.group("company", "a", None);
    assert_eq!(client.capture("still-full"), CaptureOutcome::Full);
    client.flush_blocking();
    request.assert_calls(1);
    client.flush_blocking();
    request.assert_calls(1);
    let events = seen.lock().unwrap();
    assert_eq!(events[2].properties()["$groups"], json!({"company":"a"}));
    assert_eq!(events[2].options()["process_person_profile"], true);
}

#[test]
fn hook_mutation_and_reset_keep_the_outer_context_coherent_and_reentrant_groups_fail_closed() {
    let holder: Arc<Mutex<Weak<Client>>> = Arc::new(Mutex::new(Weak::new()));
    let callback = holder.clone();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let events = seen.clone();
    let mut conf = config();
    conf.before_send(move |event| {
        let client = callback.lock().unwrap().upgrade().unwrap();
        if event.event_name() == "outer" {
            client.register(properties(json!({"value":"new"})));
            client.remove_group("company");
            client.group("recursive", "ignored", None);
            client.reset();
        }
        events.lock().unwrap().push(event);
        None
    });
    let client = Arc::new(Client::init_blocking(conf));
    *holder.lock().unwrap() = Arc::downgrade(&client);
    client.register(properties(json!({"value":"old"})));
    client.group("company", "old", None);
    client.capture("outer");
    client.capture("after");
    let events = seen.lock().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[1].properties()["value"], "old");
    assert_eq!(events[1].properties()["$groups"], json!({"company":"old"}));
    assert_eq!(events[1].distinct_id(), "anonymous");
    assert!(!events[2].properties().contains_key("value"));
    assert!(!events[2].properties().contains_key("$groups"));
    assert_ne!(events[2].distinct_id(), "anonymous");
}

#[test]
fn blocked_group_hook_retains_snapshot_through_concurrent_reset_and_opt_out() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut conf = config();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let events = seen.clone();
    conf.before_send(move |event| {
        if event.event_name() == "$groupidentify" {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        events.lock().unwrap().push(event.clone());
        Some(event)
    });
    let client = Arc::new(Client::init_blocking(conf));
    client.register(properties(json!({"value":"old"})));
    let group_client = client.clone();
    let thread = std::thread::spawn(move || group_client.group("company", "a", None));
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    client.reset();
    client.opt_out();
    release_tx.send(()).unwrap();
    thread.join().unwrap();
    assert_eq!(client.capture("blocked"), CaptureOutcome::OptedOut);
    client.opt_in();
    client.capture("new");
    let events = seen.lock().unwrap();
    assert_eq!(events[0].properties()["$groups"], json!({"company":"a"}));
    assert_eq!(events[0].properties()["value"], "old");
    assert_eq!(events[0].distinct_id(), "anonymous");
    assert!(!events[1].properties().contains_key("$groups"));
    assert!(!events[1].properties().contains_key("value"));
    assert_eq!(events[1].options()["process_person_profile"], false);
}

#[test]
fn group_and_registered_context_reach_wire_after_hook_scrubbing() {
    let server = MockServer::start();
    let request = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .body_includes("\"event\":\"$groupidentify\"")
            .body_includes("\"distinct_id\":\"anonymous\"")
            .body_includes("\"$group_type\":\"company\"")
            .body_includes("\"$group_key\":\"a\"")
            .body_includes("\"$group_set\":{\"clear\":null}")
            .body_includes("\"list\":[null,{}]")
            .body_includes("\"process_person_profile\":true")
            .body_excludes("private")
            .body_excludes("$groups")
            .body_excludes("custom_null");
        then.status(200);
    });
    let mut conf = config();
    conf.host = server.base_url();
    conf.before_send(|mut event| {
        event.remove_prop("private");
        event.remove_prop("$groups");
        Some(event)
    });
    let client = Client::init_blocking(conf);
    client.register(properties(
        json!({"private":"secret", "list":[null,{"custom_null":null}], "custom_null":null}),
    ));
    client.group("company", "a", Some(properties(json!({"clear":null}))));
    client.flush_blocking();
    request.assert_calls(1);
}

#[test]
fn queued_context_survives_reset_restart_and_retry_without_reenrichment() {
    let directory = Directory::new();
    let server = MockServer::start();
    let mut offline = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(500);
    });
    let mut conf = directory.config();
    conf.host = server.base_url();
    let client = Client::init_blocking(conf);
    client.flush_blocking(); // Initialization readiness does not promise recovery capacity.
    client.register(properties(json!({"plan":"old"})));
    client.group("company", "old", None);
    assert!(matches!(
        client.capture("old-capture"),
        CaptureOutcome::Accepted { .. }
    ));
    client.flush_blocking();
    offline.assert_calls(1);
    client.reset();
    client.register(properties(json!({"plan":"new"})));
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    let fresh_id = client.get_distinct_id();
    drop(client);
    offline.delete();
    let recovered = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .body_includes("\"event\":\"$groupidentify\"")
            .body_includes("\"event\":\"old-capture\"")
            .body_includes("\"distinct_id\":\"anonymous\"")
            .body_includes("\"plan\":\"old\"")
            .body_includes("\"$groups\":{\"company\":\"old\"}")
            .body_excludes("\"plan\":\"new\"");
        then.status(200);
    });
    let mut conf = directory.config();
    conf.host = server.base_url();
    let (client, events) = observed(conf);
    assert_eq!(client.get_distinct_id(), fresh_id);
    client.flush_blocking();
    recovered.assert_calls(1);
    assert!(events.lock().unwrap().is_empty(), "recovery reran hooks");
    client.capture("new-capture");
    let seen = events.lock().unwrap();
    assert_eq!(seen[0].properties()["plan"], "new");
    assert!(!seen[0].properties().contains_key("$groups"));
}

#[test]
fn failed_write_preserves_live_context_then_persists_latest_revision() {
    let directory = Directory::new();
    let (client, events) = observed(directory.config());
    assert_eq!(client.flush_blocking().state, PersistenceOutcome::Persisted);
    let temporary = directory.state_path().with_file_name("state.json.tmp");
    std::fs::create_dir(&temporary).unwrap();
    client.register(properties(json!({"plan":"first"})));
    client.group("company", "a", None);
    assert_eq!(
        client.flush_blocking().state,
        PersistenceOutcome::WriteFailed
    );
    client.register(properties(json!({"plan":"latest"})));
    client.group("company", "b", None);
    client.capture("live");
    let seen = events.lock().unwrap();
    assert_eq!(seen.last().unwrap().properties()["plan"], "latest");
    assert_eq!(
        seen.last().unwrap().properties()["$groups"],
        json!({"company":"b"})
    );
    drop(seen);
    std::fs::remove_dir(temporary).unwrap();
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    assert_eq!(
        directory.state()["state"]["registered"],
        json!({"plan":"latest"})
    );
    assert_eq!(directory.state()["state"]["groups"], json!({"company":"b"}));
}

#[test]
fn expanded_legacy_context_migration_preserves_file_and_uses_memory_fallback() {
    let directory = Directory::new();
    let seed = Client::init_blocking(directory.config());
    seed.shutdown_blocking(None);
    drop(seed);
    let bytes = serde_json::to_vec(&json!({"version":3,"state":{
        "anonymous_id":"a".repeat(100),"distinct_id":"b".repeat(100),
        "identified":true,"person_processing":true,"opt_out":false
    }}))
    .unwrap();
    std::fs::write(directory.state_path(), &bytes).unwrap();
    let mut conf = directory.config();
    conf.max_state_bytes = bytes.len();
    conf.opt_out = true;
    let client = Client::init_blocking(conf);
    assert!(!client.is_disabled());
    assert_eq!(client.get_distinct_id(), "anonymous");
    assert!(client.is_opt_out());
    client.register(properties(json!({"fallback":true})));
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Unavailable
    );
    assert_eq!(std::fs::read(directory.state_path()).unwrap(), bytes);
}

#[test]
fn registered_depth_boundary_rejects_whole_update_and_preserves_other_durable_state() {
    // The state envelope, state object and registered map consume three levels
    // of serde_json's bounded decoder. Exercise both sides of the actual boundary.
    for depth in [124, 125, 150] {
        let directory = Directory::new();
        let (client, events) = observed(directory.config());
        client.register(properties(json!({"plan":"old"})));
        assert_eq!(client.flush_blocking().state, PersistenceOutcome::Persisted);
        let mut nested = json!(1);
        for _ in 0..depth {
            nested = Value::Array(vec![nested]);
        }
        let mut candidate = directory.state();
        candidate["state"]["registered"] = json!({"plan":"new", "nested":nested.clone()});
        let decodable =
            serde_json::from_slice::<Value>(&serde_json::to_vec(&candidate).unwrap()).is_ok();
        assert_eq!(decodable, depth == 124);
        client.register(HashMap::from([
            ("plan".into(), json!("new")),
            ("nested".into(), nested),
        ]));
        client.capture("after-registration");
        let seen = events.lock().unwrap();
        assert_eq!(
            seen[0].properties()["plan"],
            if decodable { "new" } else { "old" }
        );
        assert_eq!(seen[0].properties().contains_key("nested"), decodable);
        drop(seen);
        assert_eq!(client.flush_blocking().state, PersistenceOutcome::Persisted);
        client.group("company", "a", None);
        client.opt_out();
        assert_eq!(
            client.shutdown_blocking(None).state,
            PersistenceOutcome::Persisted
        );
        drop(client);
        let (client, _) = observed(directory.config());
        assert!(client.is_opt_out());
        assert_eq!(directory.state()["state"]["groups"], json!({"company":"a"}));
        assert_eq!(
            directory.state()["state"]["registered"]["plan"],
            if decodable { "new" } else { "old" }
        );
    }
}

#[cfg(feature = "async-client")]
#[test]
fn async_initialization_and_drains_share_synchronous_context_mutations() {
    futures::executor::block_on(async {
        let mut conf = config();
        conf.before_send(|event| {
            assert_eq!(event.properties()["plan"], "pro");
            None
        });
        let client = Client::init(conf).await;
        client.register(properties(json!({"plan":"pro"})));
        client.group("company", "a", None);
        assert_eq!(client.capture("capture"), CaptureOutcome::Dropped);
        assert_eq!(client.flush().await.state, PersistenceOutcome::MemoryOnly);
        client.shutdown(None).await;
    });
}

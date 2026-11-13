use super::*;
use crate::client::stateful::{Config, Persistence, Snapshot};
use crate::feature_flag_evaluations::EvaluatedFlagRecord;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[path = "flag_bootstrap_tests.rs"]
mod bootstrap;
#[path = "flag_listener_tests.rs"]
mod listeners;
#[path = "flag_reads_tests.rs"]
mod reads;

const WAIT: Duration = Duration::from_secs(5);

struct Server {
    host: String,
    requests: mpsc::Receiver<Value>,
    release: mpsc::Sender<()>,
    thread: thread::JoinHandle<()>,
}

impl Server {
    fn new(responses: Vec<(u16, Value)>) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let (requests_tx, requests) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            for (status, response) in responses {
                loop {
                    let deadline = Instant::now() + WAIT;
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error)
                                if error.kind() == std::io::ErrorKind::WouldBlock
                                    && Instant::now() < deadline =>
                            {
                                thread::sleep(Duration::from_millis(1))
                            }
                            Err(error) => panic!("bounded flag fixture accept: {}", error),
                        }
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream.set_read_timeout(Some(WAIT)).unwrap();
                    let mut header = Vec::new();
                    while !header.ends_with(b"\r\n\r\n") {
                        assert!(header.len() < 8192);
                        let mut byte = [0];
                        stream.read_exact(&mut byte).unwrap();
                        header.push(byte[0]);
                    }
                    let header = String::from_utf8(header).unwrap();
                    if header.starts_with("GET /array/") && header.contains("/config HTTP/1.1\r\n")
                    {
                        let body = br#"{"hasFeatureFlags":true}"#;
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                            body.len()
                        )
                        .unwrap();
                        stream.write_all(body).unwrap();
                        continue;
                    }
                    assert!(header.starts_with("POST /flags/?v=2 HTTP/1.1\r\n"));
                    let length: usize = header
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    assert!(length <= flags::MAX_REQUEST_BYTES);
                    let mut body = vec![0; length];
                    stream.read_exact(&mut body).unwrap();
                    requests_tx
                        .send(serde_json::from_slice(&body).unwrap())
                        .unwrap();
                    release_rx.recv_timeout(WAIT).unwrap();
                    let body = serde_json::to_vec(&response).unwrap();
                    write!(
                        stream,
                        "HTTP/1.1 {} Test\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                        status,
                        body.len()
                    )
                    .unwrap();
                    stream.write_all(&body).unwrap();
                    break;
                }
            }
        });
        Self {
            host,
            requests,
            release,
            thread,
        }
    }

    fn request(&self) -> Value {
        self.requests.recv_timeout(WAIT).unwrap()
    }
    fn respond(&self) {
        self.release.send(()).unwrap();
    }
    fn finish(self) {
        self.thread.join().unwrap();
    }
}

fn config(host: &str) -> Config {
    let mut config = Config::new("phc_test");
    config.preload_feature_flags = false;
    config.host = host.into();
    config.persistence = Persistence::Memory;
    config.bootstrap_anonymous_id = Some("device".into());
    config.flush_interval = Duration::from_secs(60);
    config.shutdown_timeout = Duration::from_secs(1);
    config.before_send(|_| None);
    config
}

fn record(payload: Option<Value>) -> EvaluatedFlagRecord {
    EvaluatedFlagRecord {
        enabled: true,
        variant: None,
        payload,
        id: None,
        version: None,
        reason: None,
        locally_evaluated: false,
        has_experiment: None,
        minimal_flag_called_events: false,
    }
}

fn loaded(records: HashMap<String, EvaluatedFlagRecord>, partial: bool) -> flags::LoadedFlags {
    flags::LoadedFlags {
        records,
        request_id: Some("seed".into()),
        evaluated_at: Some(1),
        partial,
        invalid_payloads: false,
    }
}

fn seed(client: &Client) {
    client
        .state
        .as_ref()
        .unwrap()
        .mutate(|state| {
            state.flags.cache = Some(Arc::new(
                FlagCache::updated(
                    None,
                    Arc::new(FlagContext::current(state, false)),
                    state.flag_load.revision,
                    loaded(
                        HashMap::from([("old".into(), record(Some(json!("old-payload"))))]),
                        false,
                    ),
                )
                .unwrap(),
            ));
        })
        .unwrap();
}

#[test]
fn public_reload_sends_exact_current_context_while_opted_out() {
    let server = Server::new(vec![(
        200,
        json!({"featureFlags":{"checkout":false},"featureFlagPayloads":{"checkout":"\"123\""},"requestId":"wire"}),
    )]);
    let mut config = config(&server.host);
    config.disable_geoip = true;
    config.opt_out = true;
    config.bootstrap_anonymous_id = None;
    config.bootstrap_identified_id = Some("person".into());
    config
        .platform_context
        .insert("not-targeting".into(), json!(true));
    let client = Client::init_blocking(config);
    let device = client.get_anonymous_id();
    client.register(HashMap::from([("also-not-targeting".into(), json!(true))]));
    client
        .state
        .as_ref()
        .unwrap()
        .mutate(|state| {
            state.groups.insert("company".into(), "acme".into());
            state
                .flags
                .person_properties
                .insert("plan".into(), json!("pro"));
            state
                .flags
                .group_properties
                .insert("company".into(), HashMap::from([("tier".into(), json!(2))]));
        })
        .unwrap();
    client.reload_feature_flags();
    let request = server.request();
    // Ordinary state access and mutation do not wait for the blocked HTTP call.
    assert_eq!(client.get_distinct_id(), "person");
    client.opt_in();
    client.register(HashMap::from([("during-http".into(), json!(true))]));
    server.respond();
    client.flush_blocking();
    server.finish();
    assert_eq!(
        request,
        json!({"api_key":"phc_test","distinct_id":"person","$anon_distinct_id":device,"$device_id":device,
        "groups":{"company":"acme"},"person_properties":{"plan":"pro"},"group_properties":{"company":{"tier":2}},"geoip_disable":true})
    );
    let state = client.state.as_ref().unwrap().snapshot();
    let cache = state.flags.cache.as_ref().unwrap();
    assert!(!cache.flags["checkout"].record.enabled);
    assert_eq!(cache.flags["checkout"].record.payload, Some(json!("123")));
    assert_eq!(cache.flags["checkout"].context.distinct_id, "person");
    assert_eq!(state.flag_load.completed.as_ref().unwrap().error, None);
}

#[test]
fn suppressed_property_change_invalidates_inflight_response_without_refreshing() {
    let server = Server::new(vec![(200, json!({"featureFlags":{"obsolete":true}}))]);
    let client = Client::init_blocking(config(&server.host));
    seed(&client);
    let old = client
        .state
        .as_ref()
        .unwrap()
        .snapshot()
        .flags
        .cache
        .clone();
    client.reload_feature_flags();
    server.request();
    client.set_person_properties_for_flags(
        HashMap::from([("plan".into(), json!("pro"))]),
        Some(false),
    );
    server.respond();
    client.flush_blocking();
    server.finish();
    let state = client.state.as_ref().unwrap().snapshot();
    assert_eq!(state.flags.cache, old);
    assert!(state.flag_load.completed.is_none());
    assert_eq!(state.flag_load.request, 1);
    assert_eq!(state.flag_load.revision, 1);
}

#[test]
fn identity_group_and_reset_changes_clear_cache_and_reject_late_results() {
    for operation in 0..4 {
        let server = Server::new(vec![
            (200, json!({"featureFlags":{"obsolete":true}})),
            (200, json!({"featureFlags":{"current":true}})),
        ]);
        let client = Client::init_blocking(config(&server.host));
        client
            .state
            .as_ref()
            .unwrap()
            .mutate(|state| {
                state.groups.insert("company".into(), "old".into());
            })
            .unwrap();
        client
            .state
            .as_ref()
            .unwrap()
            .mutate(|state| {
                state.flags.group_properties.insert(
                    "company".into(),
                    HashMap::from([("old-target".into(), json!(true))]),
                );
            })
            .unwrap();
        seed(&client);
        client.reload_feature_flags();
        server.request();
        match operation {
            0 => client.identify("person"),
            1 => client.group("company", "new", None),
            2 => client.remove_group("company"),
            _ => client.reset(),
        }
        let before_release = client.state.as_ref().unwrap().snapshot();
        server.respond();
        let current = server.request();
        let after_obsolete = client.state.as_ref().unwrap().snapshot();
        server.respond();
        client.flush_blocking();
        server.finish();
        assert_eq!(current["distinct_id"], before_release.distinct_id);
        assert_eq!(current["groups"], json!(before_release.groups));
        assert!(after_obsolete.flags.cache.is_none());
        assert!(after_obsolete.flag_load.completed.is_none());
        assert!(before_release.flags.cache.is_none());
        if operation != 0 {
            assert!(!before_release
                .flags
                .group_properties
                .contains_key("company"));
        }
        let after = client.state.as_ref().unwrap().snapshot();
        let cache = after.flags.cache.as_ref().unwrap();
        assert_eq!(cache.flags.len(), 1);
        assert!(cache.flags.contains_key("current"));
        assert!(after.flag_load.revision > 1);
    }
}

#[test]
fn concurrent_reloads_keep_only_latest_requested_context_and_active_result_is_obsolete() {
    let server = Server::new(vec![
        (
            200,
            json!({"featureFlags":{"obsolete":true},"requestId":"old"}),
        ),
        (
            200,
            json!({"featureFlags":{"latest":true},"requestId":"new"}),
        ),
    ]);
    let client = Arc::new(Client::init_blocking(config(&server.host)));
    client.reload_feature_flags();
    server.request();
    let threads: Vec<_> = (0..4)
        .map(|i| {
            let client = client.clone();
            thread::spawn(move || {
                for j in 0..25 {
                    client
                        .state
                        .as_ref()
                        .unwrap()
                        .mutate(|state| {
                            state
                                .flags
                                .person_properties
                                .insert("version".into(), json!([i, j]));
                        })
                        .unwrap();
                    client.reload_feature_flags();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    client
        .state
        .as_ref()
        .unwrap()
        .mutate(|state| {
            state
                .flags
                .person_properties
                .insert("version".into(), json!("latest"));
        })
        .unwrap();
    client.reload_feature_flags();
    server.respond();
    let latest = server.request();
    // Active response is obsolete even before the pending response completes.
    let before_release = client.state.as_ref().unwrap().snapshot();
    server.respond();
    client.flush_blocking();
    server.finish();
    assert!(before_release.flags.cache.is_none());
    assert!(before_release.flag_load.completed.is_none());
    assert_eq!(latest["person_properties"]["version"], "latest");
    let state = client.state.as_ref().unwrap().snapshot();
    assert_eq!(state.flag_load.request, 102);
    let cache = state.flags.cache.as_ref().unwrap();
    assert_eq!(cache.flags.len(), 1);
    assert_eq!(cache.flags["latest"].request_id.as_deref(), Some("new"));
    assert_eq!(cache.context.person_properties["version"], "latest");
}

#[test]
fn partial_merge_preserves_record_provenance_and_restore_filters_stale_records() {
    let server = Server::new(vec![
        (
            200,
            json!({"flags":{
            "a":{"key":"a","enabled":true,"metadata":{"id":1,"version":1,"payload":"\"first\"","has_experiment":false}},
            "b":{"key":"b","enabled":true,"metadata":{"id":2,"version":1,"payload":"null"}}
        },"requestId":"first","evaluatedAt":10,"minimalFlagCalledEvents":true}),
        ),
        (
            200,
            json!({"flags":{
            "a":{"key":"a","enabled":false,"failed":true},
            "b":{"key":"b","enabled":false,"metadata":{"id":2,"version":2,"payload":"false"}},
            "c":{"key":"c","enabled":true}
        },"requestId":"second","evaluatedAt":20}),
        ),
        (200, json!({"featureFlags":{},"requestId":"empty"})),
    ]);
    let client = Client::init_blocking(config(&server.host));
    client.reload_feature_flags();
    server.request();
    server.respond();
    client.flush_blocking();
    let first = client.state.as_ref().unwrap().snapshot();
    client
        .state
        .as_ref()
        .unwrap()
        .mutate(|state| {
            state
                .flags
                .person_properties
                .insert("plan".into(), json!("pro"));
        })
        .unwrap();
    client.reload_feature_flags();
    server.request();
    server.respond();
    client.flush_blocking();
    let second = client.state.as_ref().unwrap().snapshot();
    let cache = second.flags.cache.as_ref().unwrap();
    assert_eq!(cache.flags.len(), 3);
    assert_eq!(
        cache.flags["a"],
        first.flags.cache.as_ref().unwrap().flags["a"]
    );
    assert_eq!(cache.flags["a"].request_id.as_deref(), Some("first"));
    assert_eq!(cache.flags["a"].record.payload, Some(json!("first")));
    assert!(cache.flags["a"].record.minimal_flag_called_events);
    assert_eq!(cache.flags["b"].record.payload, Some(json!(false)));
    assert!(!cache.flags["b"].record.enabled);
    assert!(!cache.flags["b"].record.minimal_flag_called_events);
    assert_eq!(cache.flags["b"].request_id.as_deref(), Some("second"));
    assert_eq!(cache.flags["b"].evaluated_at, Some(20));
    assert!(cache.flags["b"].partial);
    assert_eq!(cache.flags["b"].context.person_properties["plan"], "pro");
    assert_eq!(cache.flags["b"].revision, 1);
    let mut restored: Snapshot =
        serde_json::from_slice(&serde_json::to_vec(second.as_ref()).unwrap()).unwrap();
    assert_eq!(restored.flag_load.revision, 0);
    assert!(restored.reconcile_flag_cache(false));
    let restored = restored.flags.cache.as_ref().unwrap();
    assert!(!restored.flags.contains_key("a"));
    assert_eq!(restored.flags["b"], cache.flags["b"]);
    client.reload_feature_flags();
    server.request();
    server.respond();
    client.flush_blocking();
    server.finish();
    let final_state = client.state.as_ref().unwrap().snapshot();
    assert!(final_state.flags.cache.as_ref().unwrap().flags.is_empty());
    assert!(!final_state.flag_load.completed.as_ref().unwrap().partial);
    // Earlier immutable snapshots remain coherent after replacement.
    assert_eq!(first.flags.cache.as_ref().unwrap().flags.len(), 2);
    assert_eq!(second.flags.cache.as_ref().unwrap().flags.len(), 3);
}

#[test]
fn failures_retain_cache_and_completion_records_error_for_the_request() {
    let server = Server::new(vec![
        (503, json!({})),
        (200, json!({"quotaLimited":["feature_flags"]})),
        (200, json!({"flags":null})),
    ]);
    let client = Client::init_blocking(config(&server.host));
    seed(&client);
    let initial = client
        .state
        .as_ref()
        .unwrap()
        .snapshot()
        .flags
        .cache
        .clone();
    for error in [
        flags::LoadError::Http(503),
        flags::LoadError::QuotaLimited,
        flags::LoadError::InvalidResponse,
    ] {
        client.reload_feature_flags();
        server.request();
        server.respond();
        client.flush_blocking();
        let state = client.state.as_ref().unwrap().snapshot();
        assert_eq!(state.flags.cache, initial);
        assert_eq!(
            state.flag_load.completed.as_ref().unwrap().error,
            Some(error)
        );
        assert_eq!(
            state.flag_load.completed.as_ref().unwrap().request,
            state.flag_load.request
        );
    }
    server.finish();
}

#[test]
fn close_from_capture_hook_rejects_active_flag_completion() {
    let server = Server::new(vec![(200, json!({"featureFlags":{"late":true}}))]);
    let weak = Arc::new(Mutex::new(std::sync::Weak::<Client>::new()));
    let in_hook = weak.clone();
    let mut config = config(&server.host);
    // Replace the event-dropping fixture hook so closure actually runs.
    config.before_send.clear();
    config.before_send(move |event| {
        if let Some(client) = in_hook.lock().unwrap().upgrade() {
            assert_eq!(
                client.shutdown_blocking(Some(Duration::ZERO)).delivery,
                crate::client::stateful::DeliveryOutcome::Deferred
            );
        }
        Some(event)
    });
    let client = Arc::new(Client::init_blocking(config));
    *weak.lock().unwrap() = Arc::downgrade(&client);
    client.reload_feature_flags();
    server.request();
    let outcome = client.capture("close");
    server.respond();
    client.shutdown_blocking(None);
    server.finish();
    assert_eq!(outcome, crate::client::stateful::CaptureOutcome::Closed);
    let state = client.state.as_ref().unwrap().snapshot();
    assert!(state.flags.cache.is_none());
    assert!(state.flag_load.completed.is_none());
}

#[test]
fn live_cache_bound_rejects_partial_expansion_atomically_and_preserves_mandatory_updates() {
    let client = Client::init_blocking(config("http://127.0.0.1:1"));
    let writer = client.state.as_ref().unwrap();
    let request = Request {
        context: Arc::new(FlagContext::current(&writer.snapshot(), false)),
        revision: 0,
        sequence: 0,
    };
    let chunk = |prefix: &str| {
        (0..1000)
            .map(|i| {
                (
                    format!("{prefix}-{i}"),
                    record(Some(json!("x".repeat(1024)))),
                )
            })
            .collect()
    };
    assert_eq!(
        install(
            &writer.access(),
            &request,
            Ok(loaded(chunk("first"), false))
        ),
        None
    );
    let previous = writer.snapshot().flags.cache.clone();
    assert_eq!(previous.as_ref().unwrap().flags.len(), 1000);
    assert_eq!(
        install(
            &writer.access(),
            &request,
            Ok(loaded(chunk("second"), true))
        ),
        Some(flags::LoadError::CacheTooLarge)
    );
    assert_eq!(writer.snapshot().flags.cache, previous);
    client.opt_out();
    client.register(HashMap::from([("after-overflow".into(), json!(true))]));
    assert!(client.is_opt_out());
    assert_eq!(writer.snapshot().registered["after-overflow"], true);
    let mut amplified = loaded(HashMap::from([("huge".into(), record(None))]), true);
    amplified.request_id = Some("x".repeat(flags::MAX_RESPONSE_BYTES));
    assert_eq!(
        install(&writer.access(), &request, Ok(amplified)),
        Some(flags::LoadError::CacheTooLarge)
    );
    assert_eq!(writer.snapshot().flags.cache, previous);
    assert_eq!(
        install(
            &writer.access(),
            &request,
            Ok(loaded(HashMap::new(), false))
        ),
        None
    );
    assert!(writer
        .snapshot()
        .flags
        .cache
        .as_ref()
        .unwrap()
        .flags
        .is_empty());
}

#[test]
fn request_size_error_hooks_can_mutate_and_reload_without_a_lock_cycle() {
    let server = Server::new(vec![(200, json!({"featureFlags":{"after-error":true}}))]);
    let weak = Arc::new(Mutex::new(std::sync::Weak::<Client>::new()));
    let on_error = weak.clone();
    let mut config = config(&server.host);
    config.max_state_bytes = 4 * 1024 * 1024;
    config.on_error(move |_| {
        let client = on_error.lock().unwrap().upgrade().unwrap();
        client
            .state
            .as_ref()
            .unwrap()
            .mutate(|state| state.flags.person_properties.clear())
            .unwrap();
        client.reload_feature_flags();
    });
    let client = Arc::new(Client::init_blocking(config));
    *weak.lock().unwrap() = Arc::downgrade(&client);
    client
        .state
        .as_ref()
        .unwrap()
        .mutate(|state| {
            state.flags.person_properties.insert(
                "oversized".into(),
                json!("x".repeat(flags::MAX_REQUEST_BYTES)),
            );
        })
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let reloading = client.clone();
    let reload_thread = thread::spawn(move || {
        reloading.reload_feature_flags();
        done_tx.send(()).unwrap();
    });
    let completed = done_rx.recv_timeout(WAIT);
    let request = server.request();
    server.respond();
    assert!(completed.is_ok());
    reload_thread.join().unwrap();
    client.flush_blocking();
    server.finish();
    assert_eq!(request["person_properties"], json!({}));
    assert!(client
        .state
        .as_ref()
        .unwrap()
        .snapshot()
        .flags
        .cache
        .as_ref()
        .unwrap()
        .flags
        .contains_key("after-error"));
}

#[test]
fn evaluation_revision_changes_only_with_effective_context_and_rejected_mutations_are_atomic() {
    let client = Client::init_blocking(config("http://127.0.0.1:1"));
    client.flush_blocking();
    seed(&client);
    client.register(HashMap::from([("analytics-only".into(), json!(1))]));
    client.opt_out();
    client.opt_in();
    client.session_id();
    client.identify("device"); // Identified bit alone is not an evaluation input.
    let writer = client.state.as_ref().unwrap();
    assert_eq!(writer.snapshot().flag_load.revision, 0);
    assert!(writer.snapshot().flags.cache.is_some());
    writer
        .mutate(|state| state.flags.person_properties.insert("p".into(), json!(1)))
        .unwrap();
    assert_eq!(writer.snapshot().flag_load.revision, 1);
    writer
        .mutate(|state| state.flags.person_properties.insert("p".into(), json!(1)))
        .unwrap();
    assert_eq!(writer.snapshot().flag_load.revision, 1);
    let before = writer.snapshot();
    assert!(writer
        .mutate(|state| {
            state.distinct_id = "rejected".into();
            state
                .flags
                .person_properties
                .insert("oversized".into(), json!("x".repeat(2 * 1024 * 1024)));
        })
        .is_err());
    assert_eq!(writer.snapshot(), before);
    writer
        .mutate(|state| {
            state
                .flags
                .group_properties
                .insert("company".into(), HashMap::from([("p".into(), json!(1))]))
        })
        .unwrap();
    assert_eq!(writer.snapshot().flag_load.revision, 2);
    client.reset();
    assert_eq!(writer.snapshot().flag_load.revision, 3);
    assert!(writer.snapshot().flags.cache.is_none());
    let disabled = Client::init_blocking(Config::new(""));
    disabled.reload_feature_flags();
    client.shutdown_blocking(None);
    client.reload_feature_flags();
}

#[test]
fn refresh_sequence_and_reset_revision_each_prevent_obsolete_installation() {
    for reset in [false, true] {
        let server = Server::new(vec![
            (200, json!({"featureFlags":{"old":true}})),
            (200, json!({"featureFlags":{"new":true}})),
        ]);
        let client = Client::init_blocking(config(&server.host));
        client.reload_feature_flags();
        server.request();
        let first = client.state.as_ref().unwrap().snapshot();
        if reset {
            client.reset();
        } else {
            client.reload_feature_flags();
        }
        let pending = client.state.as_ref().unwrap().snapshot();
        if reset {
            assert_eq!(first.flag_load.request, pending.flag_load.request);
            assert_ne!(first.flag_load.revision, pending.flag_load.revision);
        } else {
            assert_ne!(first.flag_load.request, pending.flag_load.request);
            assert_eq!(first.flag_load.revision, pending.flag_load.revision);
        }
        server.respond();
        server.request();
        let intermediate = client.state.as_ref().unwrap().snapshot();
        server.respond();
        client.flush_blocking();
        server.finish();
        assert!(intermediate.flags.cache.is_none());
        assert!(intermediate.flag_load.completed.is_none());
        let cache = client
            .state
            .as_ref()
            .unwrap()
            .snapshot()
            .flags
            .cache
            .clone()
            .unwrap();
        assert_eq!(cache.flags.len(), 1);
        assert!(cache.flags.contains_key("new"));
    }
}

#[test]
fn returning_to_the_original_targeting_does_not_revive_an_obsolete_response() {
    let server = Server::new(vec![(200, json!({"featureFlags":{"old":true}}))]);
    let client = Client::init_blocking(config(&server.host));
    client.reload_feature_flags();
    server.request();
    let writer = client.state.as_ref().unwrap();
    client.set_person_properties_for_flags(
        HashMap::from([("plan".into(), json!("pro"))]),
        Some(false),
    );
    client.reset_person_properties_for_flags(Some(false));
    server.respond();
    client.flush_blocking();
    server.finish();
    assert_eq!(writer.snapshot().flag_load.revision, 2);
    assert!(writer.snapshot().flags.cache.is_none());
}

#[test]
fn public_restart_hydrates_only_records_matching_persisted_targeting() {
    let path = std::env::temp_dir().join(format!("posthog-flag-restore-{}", uuid::Uuid::now_v7()));
    let server = Server::new(vec![
        (
            200,
            json!({"featureFlags":{"old":true},"requestId":"first"}),
        ),
        (
            200,
            json!({"featureFlags":{"fresh":false},"featureFlagPayloads":{"fresh":null},"errorsWhileComputingFlags":true,"requestId":"second"}),
        ),
    ]);
    let make_config = || {
        let mut config = config(&server.host);
        config.persistence = Persistence::Directory(path.clone());
        config
    };
    let client = Client::init_blocking(make_config());
    client.reload_feature_flags();
    server.request();
    server.respond();
    client.flush_blocking();
    client
        .state
        .as_ref()
        .unwrap()
        .mutate(|state| {
            state
                .flags
                .person_properties
                .insert("plan".into(), json!("pro"))
        })
        .unwrap();
    client.reload_feature_flags();
    server.request();
    server.respond();
    client.flush_blocking();
    assert_eq!(
        client
            .state
            .as_ref()
            .unwrap()
            .snapshot()
            .flags
            .cache
            .as_ref()
            .unwrap()
            .flags
            .len(),
        2
    );
    assert_eq!(
        client.shutdown_blocking(None).state,
        crate::client::stateful::PersistenceOutcome::Persisted
    );
    drop(client);
    let restored = Client::init_blocking(make_config());
    let snapshot = restored.state.as_ref().unwrap().snapshot();
    let cache = snapshot.flags.cache.as_ref().unwrap();
    assert_eq!(cache.flags.len(), 1);
    assert!(!cache.flags["fresh"].record.enabled);
    assert_eq!(cache.flags["fresh"].record.payload, Some(Value::Null));
    assert_eq!(cache.flags["fresh"].request_id.as_deref(), Some("second"));
    assert_eq!(
        cache.flags["fresh"].context.person_properties["plan"],
        "pro"
    );
    assert_eq!(
        restored.shutdown_blocking(None).state,
        crate::client::stateful::PersistenceOutcome::Persisted
    );
    drop(restored);
    server.finish();
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn fitting_partial_replacements_are_admitted_in_either_hash_iteration_order() {
    let context = Arc::new(FlagContext::current(
        &Snapshot::anonymous("device".into()),
        false,
    ));
    for grows_first in [false, true] {
        let mut replacements = HashMap::from([
            ("a".to_string(), record(None)),
            ("b".to_string(), record(None)),
        ]);
        // Select sizes from the actual iteration order, without changing its keys
        // or capacity. Both paths are deterministic regardless of randomized hash seeds.
        let keys: Vec<_> = replacements.keys().cloned().collect();
        let grow = &keys[usize::from(!grows_first)];
        let shrink = &keys[usize::from(grows_first)];
        let old = HashMap::from([
            (grow.clone(), record(Some(json!("x".repeat(400 * 1024))))),
            (shrink.clone(), record(Some(json!("x".repeat(1400 * 1024))))),
        ]);
        let previous = FlagCache::updated(None, context.clone(), 0, loaded(old, false)).unwrap();
        replacements.get_mut(grow).unwrap().payload = Some(json!("x".repeat(800 * 1024)));
        replacements.get_mut(shrink).unwrap().payload = Some(json!(""));
        let updated = FlagCache::updated(
            Some(&previous),
            context.clone(),
            1,
            loaded(replacements, true),
        )
        .expect("fitting partial replacements must not depend on iteration order");
        assert_eq!(updated.flags.len(), 2);
        assert_eq!(
            updated.flags[grow]
                .record
                .payload
                .as_ref()
                .unwrap()
                .as_str()
                .unwrap()
                .len(),
            800 * 1024
        );
        assert_eq!(updated.flags[shrink].record.payload, Some(json!("")));
        assert!(updated.within_limits());
        assert_eq!(
            previous.flags[shrink]
                .record
                .payload
                .as_ref()
                .unwrap()
                .as_str()
                .unwrap()
                .len(),
            1400 * 1024
        );
    }
}

fn preload_config(host: &str) -> Config {
    let mut config = Config::new("phc_test");
    assert!(config.preload_feature_flags);
    config.host = host.into();
    config.persistence = Persistence::Memory;
    config.bootstrap_anonymous_id = Some("device".into());
    config.shutdown_timeout = Duration::from_secs(1);
    config
}

fn preload_readiness(asynchronous: bool) {
    let server = Server::new(vec![(
        200,
        json!({"featureFlags":{"ready":false},"featureFlagPayloads":{"ready":null}}),
    )]);
    let mut config = preload_config(&server.host);
    config.opt_out = true;
    config.person_profiles = crate::stateful::PersonProfiles::Never;
    config.before_send(|_| panic!("preload must not emit analytics"));
    let (tx, rx) = mpsc::channel();
    let initializing = thread::spawn(move || {
        #[cfg(feature = "async-client")]
        if asynchronous {
            tx.send(futures::executor::block_on(Client::init(config)))
                .unwrap();
            return;
        }
        assert!(!asynchronous);
        tx.send(Client::init_blocking(config)).unwrap();
    });
    let request = server.request();
    // The response remains gated while both constructors must return ready.
    let ready = rx.recv_timeout(WAIT);
    if let Ok(client) = &ready {
        assert!(!client.is_disabled());
        assert!(client
            .state
            .as_ref()
            .unwrap()
            .snapshot()
            .flags
            .cache
            .is_none());
        client.register(HashMap::from([("during-preload".into(), json!(true))]));
        assert_eq!(
            client.capture("out"),
            crate::stateful::CaptureOutcome::OptedOut
        );
    }
    server.respond();
    initializing.join().unwrap();
    let client = ready.expect("initialization must not wait for a flags response");
    client.flush_blocking();
    server.finish();
    assert_eq!(
        request,
        json!({"api_key":"phc_test", "distinct_id":"device",
        "$anon_distinct_id":"device", "$device_id":"device", "geoip_disable":false,
        "person_properties":{}, "group_properties":{}, "groups":{}})
    );
    let snapshot = client.state.as_ref().unwrap().snapshot();
    assert_eq!(snapshot.flag_load.request, 1);
    let record = &snapshot.flags.cache.as_ref().unwrap().flags["ready"].record;
    assert!(!record.enabled);
    assert_eq!(record.payload, Some(Value::Null));
    assert!(client.is_opt_out());
    assert_eq!(client.peek_session_id(), None);
}

#[test]
fn default_preload_does_not_delay_blocking_readiness_or_require_analytics_consent() {
    preload_readiness(false);
}

#[cfg(feature = "async-client")]
#[test]
fn default_preload_does_not_delay_runtime_free_async_readiness() {
    preload_readiness(true);
}

#[test]
fn preload_uses_restored_targeting_after_identified_bootstrap_reconciliation() {
    let server = Server::new(vec![(200, json!({"featureFlags":{"current":true}}))]);
    let directory = std::env::temp_dir().join(format!("posthog-preload-{}", uuid::Uuid::now_v7()));
    let mut initial = preload_config(&server.host);
    initial.preload_feature_flags = false;
    initial.persistence = Persistence::Directory(directory.clone());
    let client = Client::init_blocking(initial);
    client.set_person_properties_for_flags(
        HashMap::from([("plan".into(), json!("pro"))]),
        Some(false),
    );
    seed(&client);
    assert_eq!(
        client.shutdown_blocking(None).state,
        crate::stateful::PersistenceOutcome::Persisted
    );
    drop(client);
    let mut next = preload_config(&server.host);
    next.persistence = Persistence::Directory(directory.clone());
    next.bootstrap_anonymous_id = None;
    next.bootstrap_identified_id = Some("person".into());
    next.opt_out = true;
    let client = Client::init_blocking(next);
    let request = server.request();
    let before = client.state.as_ref().unwrap().snapshot();
    server.respond();
    client.flush_blocking();
    server.finish();
    assert_eq!(request["distinct_id"], "person");
    assert_eq!(request["$anon_distinct_id"], "device");
    assert_eq!(request["$device_id"], "device");
    assert_eq!(request["person_properties"], json!({"plan":"pro"}));
    assert!(before.flags.cache.is_none());
    assert_eq!(before.flag_load.request, 1);
    assert!(client
        .state
        .as_ref()
        .unwrap()
        .snapshot()
        .flags
        .cache
        .as_ref()
        .unwrap()
        .flags
        .contains_key("current"));
    client.shutdown_blocking(None);
    drop(client);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn preload_failure_reports_without_disabling_the_ready_client() {
    let server = Server::new(vec![(503, json!({}))]);
    let mut config = preload_config(&server.host);
    let (tx, rx) = mpsc::channel();
    config.on_error(move |error| {
        tx.send(matches!(error, crate::PostHogError::FeatureFlags(_)))
            .unwrap();
    });
    let client = Client::init_blocking(config);
    server.request();
    assert!(!client.is_disabled());
    server.respond();
    client.flush_blocking();
    server.finish();
    assert!(rx.recv_timeout(WAIT).unwrap());
    assert!(!client.is_disabled());
    assert_eq!(
        client
            .state
            .as_ref()
            .unwrap()
            .snapshot()
            .flag_load
            .completed
            .as_ref()
            .unwrap()
            .error,
        Some(flags::LoadError::Http(503))
    );
}

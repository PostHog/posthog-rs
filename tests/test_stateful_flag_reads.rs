#![cfg(feature = "stateful")]

use httpmock::{Method::POST, MockServer};
use posthog::stateful::{Client, Config, Persistence, PersonProfiles};
use posthog::FlagValue;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

fn config(server: &MockServer) -> Config {
    let mut config = Config::new("phc_test");
    config.host = server.base_url();
    config.persistence = Persistence::Memory;
    config.preload_feature_flags = false;
    config.bootstrap_anonymous_id = Some("device".into());
    config.shutdown_timeout = Duration::from_secs(1);
    config
}

fn reload(client: &Client) {
    let (tx, rx) = std::sync::mpsc::channel();
    let subscription = client.on_feature_flags(move |_| {
        let _ = tx.send(());
    });
    // Registration replays the existing cache before a new request is scheduled.
    while rx.try_recv().is_ok() {}
    client.reload_feature_flags();
    rx.recv_timeout(Duration::from_secs(5))
        .expect("flags completion");
    subscription.unsubscribe();
}

fn bootstrap(config: &mut Config) {
    config.bootstrap_feature_flags = Some(HashMap::from([
        ("variant".into(), FlagValue::String("blue".into())),
        ("nullable".into(), FlagValue::Boolean(true)),
        ("plain".into(), FlagValue::Boolean(true)),
        ("disabled".into(), FlagValue::Boolean(false)),
        ("empty".into(), FlagValue::String(String::new())),
    ]));
    config.bootstrap_feature_flag_payloads = Some(HashMap::from([
        ("variant".into(), json!("123")),
        ("nullable".into(), Value::Null),
        ("disabled".into(), json!(false)),
        ("orphan".into(), json!({"ignored":true})),
    ]));
}

#[test]
fn complete_failed_records_replace_cache_while_response_errors_merge_successes() {
    for partial in [false, true] {
        let server = MockServer::start();
        let mut response = server.mock(|when, then| {
            when.path("/flags/");
            then.status(200).json_body(json!({
                "flags": {
                    "variant": {"failed":true},
                    "healthy": {"key":"healthy", "enabled":true, "metadata":{"id":7,"version":1,"payload":"null"}},
                    "disabled": {"key":"disabled", "enabled":false}
                },
                "errorsWhileComputingFlags":partial,
                "requestId":"replacement"
            }));
        });
        let mut settings = config(&server);
        bootstrap(&mut settings);
        let client = Client::init_blocking(settings);
        let (tx, rx) = std::sync::mpsc::channel();
        let subscription = client.on_feature_flags(move |update| {
            let _ = tx.send(update.clone());
        });
        rx.recv_timeout(Duration::from_secs(5)).unwrap(); // Bootstrap snapshot.
        client.reload_feature_flags();
        let update = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(update.partial, partial);
        assert_eq!(update.errors_loading, partial);
        assert_eq!(update.error, None);
        let flags = client.get_feature_flags().unwrap();
        assert_eq!(flags.len(), if partial { 5 } else { 2 });
        assert_eq!(flags.contains_key("variant"), partial);
        assert_eq!(flags.contains_key("plain"), partial);
        assert_eq!(flags["healthy"].id, Some(7));
        assert_eq!(flags["healthy"].payload, Some(Value::Null));
        assert_eq!(flags["healthy"].request_id.as_deref(), Some("replacement"));
        assert_eq!(flags["healthy"].partial, partial);
        assert!(!flags["disabled"].enabled);
        let enabled = update.flags.unwrap();
        assert!(enabled.contains_key("healthy"));
        assert!(!enabled.contains_key("disabled"));
        assert_eq!(enabled.contains_key("variant"), partial);
        if partial {
            assert_eq!(flags["variant"].variant.as_deref(), Some("blue"));
            assert_eq!(flags["variant"].request_id, None);
            assert!(!flags["variant"].partial);
        }
        response.assert_calls(1);
        response.delete();
        let empty = server.mock(|when, then| {
            when.path("/flags/");
            then.status(200)
                .json_body(json!({"flags":{"healthy":{"failed":true}}}));
        });
        client.reload_feature_flags();
        let update = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!update.partial);
        assert!(!update.errors_loading);
        assert_eq!(update.flags, Some(HashMap::new()));
        assert_eq!(client.get_feature_flags(), Some(HashMap::new()));
        assert_eq!(client.feature_flag("healthy").get(), None);
        empty.assert_calls(1);
        subscription.unsubscribe();
        client.shutdown_blocking(None);
    }
}

#[test]
fn bootstrap_rich_reads_are_owned_memory_only_and_silent_in_all_consent_modes() {
    for opted_out in [false, true] {
        let server = MockServer::start();
        let requests = server.mock(|when, then| {
            when.path_not("/array/phc_test/config");
            then.status(500);
        });
        let mut config = config(&server);
        bootstrap(&mut config);
        config.opt_out = opted_out;
        config.person_profiles = PersonProfiles::Never;
        let hooks = Arc::new(AtomicUsize::new(0));
        let seen = hooks.clone();
        config.before_send(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            None
        });
        let client = Client::init_blocking(config);
        let expected = client.get_feature_flags().unwrap();
        assert_eq!(expected.len(), 3);
        assert!(expected["variant"].enabled);
        assert_eq!(expected["variant"].variant.as_deref(), Some("blue"));
        assert_eq!(expected["variant"].payload, Some(json!("123")));
        assert_eq!(expected["nullable"].payload, Some(Value::Null));
        assert_eq!(expected["plain"].payload, None);
        for _ in 0..10 {
            assert_eq!(client.get_feature_flags(), Some(expected.clone()));
            for (key, result) in &expected {
                assert_eq!(
                    client.feature_flag(key).send_event(false).get().as_ref(),
                    Some(result)
                );
                assert_eq!(result.key, *key);
                assert_eq!(result.context.distinct_id(), "device");
            }
            for key in ["missing", "disabled", "empty", "orphan"] {
                assert_eq!(client.feature_flag(key).send_event(false).get(), None);
                assert!(!expected.contains_key(key));
            }
        }
        let mut results = client.get_feature_flags().unwrap();
        results.get_mut("variant").unwrap().payload = Some(json!("changed"));
        results.clear();
        let mut result = client
            .feature_flag("variant")
            .send_event(false)
            .get()
            .unwrap();
        result.variant = Some("changed".into());
        assert_eq!(client.get_feature_flags(), Some(expected));
        assert_eq!(
            client
                .feature_flag("variant")
                .send_event(false)
                .get()
                .unwrap()
                .payload,
            Some(json!("123"))
        );
        assert_eq!(client.peek_session_id(), None);
        assert_eq!(client.is_opt_out(), opted_out);
        client.flush_blocking();
        requests.assert_calls(0);
        assert_eq!(hooks.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn lookup_reads_current_cache_and_reports_only_when_get_is_called() {
    let server = MockServer::start();
    let response = server.mock(|when, then| {
        when.path("/flags/");
        then.status(200)
            .json_body(json!({"featureFlags":{"variant":"green","disabled":false}}));
    });
    let analytics = server.mock(|when, then| {
        when.path("/i/v1/analytics/events");
        then.status(200);
    });
    let mut config = config(&server);
    bootstrap(&mut config);
    let hooks = Arc::new(AtomicUsize::new(0));
    let seen = hooks.clone();
    config.before_send(move |event| {
        seen.fetch_add(1, Ordering::SeqCst);
        Some(event)
    });
    let client = Client::init_blocking(config);
    let lookup = client.feature_flag("variant");
    assert_eq!(hooks.load(Ordering::SeqCst), 0);
    reload(&client);
    response.assert_calls(1);
    analytics.assert_calls(0);
    assert_eq!(hooks.load(Ordering::SeqCst), 0);
    assert_eq!(lookup.get().unwrap().variant.as_deref(), Some("green"));
    assert_eq!(hooks.load(Ordering::SeqCst), 1);
    assert!(!client
        .feature_flag("disabled")
        .send_event(true)
        .send_event(false)
        .get()
        .map_or(true, |flag| flag.enabled));
    assert_eq!(hooks.load(Ordering::SeqCst), 1);
    assert!(
        !client
            .feature_flag("disabled")
            .send_event(true)
            .get()
            .unwrap()
            .enabled
    );
    assert_eq!(hooks.load(Ordering::SeqCst), 2);
    assert!(client
        .feature_flag("missing")
        .get()
        .map_or(true, |flag| flag.enabled));
    assert_eq!(
        client
            .feature_flag("missing")
            .get()
            .and_then(|flag| flag.variant)
            .unwrap_or_else(|| "control".into()),
        "control"
    );
    // A missing flag is reported once, like any other value.
    assert_eq!(hooks.load(Ordering::SeqCst), 3);
    client.flush_blocking();
    analytics.assert_calls(1);
}

#[test]
fn v2_rich_reads_preserve_disabled_values_and_decoded_payload_types_without_reparsing() {
    let server = MockServer::start();
    let payloads = [
        Value::Null,
        json!(false),
        json!(0),
        json!(""),
        json!("123"),
        json!("true"),
        json!([1, null]),
        json!({"nested":null}),
    ];
    let mut flags: HashMap<String, Value> = payloads
        .iter()
        .enumerate()
        .map(|(i, value)| {
            let key = format!("flag-{i}");
            (
                key.clone(),
                json!({"key":key,"enabled":true,"variant":"blue",
            "metadata":{"id":i,"version":1,"payload":serde_json::to_string(value).unwrap()}}),
            )
        })
        .collect();
    flags.insert("disabled".into(), json!({"key":"disabled","enabled":false,"variant":"ignored", "metadata":{"id":10,"version":1,"payload":"null"}}));
    flags.insert(
        "bad".into(),
        json!({"key":"bad","enabled":true,"metadata":{"id":11,"version":1,"payload":"{broken"}}),
    );
    let response = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({"flags":flags}));
    });
    let analytics = server.mock(|when, then| {
        when.path("/i/v1/analytics/events");
        then.status(200);
    });
    let mut config = config(&server);
    let errors = Arc::new(AtomicUsize::new(0));
    let seen = errors.clone();
    config.on_error(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    let client = Client::init_blocking(config);
    reload(&client);
    let errors_before = errors.load(Ordering::SeqCst);
    assert!(
        errors_before > 0,
        "malformed wire payload is diagnosed during load"
    );
    let results = client.get_feature_flags().unwrap();
    assert!(!results["disabled"].enabled);
    assert_eq!(results["disabled"].variant, None);
    assert_eq!(results["disabled"].payload, Some(Value::Null));
    assert!(results["bad"].enabled);
    assert_eq!(results["bad"].payload, None);
    assert!(!results.contains_key("missing"));
    for (i, payload) in payloads.iter().enumerate() {
        let key = format!("flag-{i}");
        let result = &results[&key];
        assert_eq!(result.variant.as_deref(), Some("blue"));
        assert_eq!(result.payload.as_ref(), Some(payload));
        assert_eq!(result.id, Some(i as u64));
        assert_eq!(result.version, Some(1));
    }
    for (key, result) in &results {
        assert_eq!(
            client.feature_flag(key).send_event(false).get().as_ref(),
            Some(result)
        );
    }
    client.flush_blocking();
    response.assert_calls(1);
    analytics.assert_calls(0);
    assert_eq!(errors.load(Ordering::SeqCst), errors_before);
    assert_eq!(client.peek_session_id(), None);
}

#[test]
fn legacy_http_payloads_decode_once_before_rich_reads() {
    let server = MockServer::start();
    let payloads = [
        Value::Null,
        json!(false),
        json!(0),
        json!(""),
        json!("123"),
        json!("true"),
        json!([1, null]),
        json!({"nested":null}),
    ];
    let mut flags = serde_json::Map::new();
    let mut wire_payloads = serde_json::Map::new();
    for (index, payload) in payloads.iter().enumerate() {
        let key = format!("flag-{index}");
        flags.insert(key.clone(), json!(true));
        wire_payloads.insert(key, json!(serde_json::to_string(payload).unwrap()));
    }
    flags.insert("bad".into(), json!(true));
    wire_payloads.insert("bad".into(), json!("{broken"));
    let response = server.mock(|when, then| {
        when.path("/flags/");
        then.status(200).json_body(json!({"featureFlags":flags,
            "featureFlagPayloads":wire_payloads,"requestId":"legacy"}));
    });
    let analytics = server.mock(|when, then| {
        when.path("/i/v1/analytics/events");
        then.status(200);
    });
    let client = Client::init_blocking(config(&server));
    reload(&client);
    let all = client.get_feature_flags().unwrap();
    for (index, payload) in payloads.iter().enumerate() {
        let key = format!("flag-{index}");
        let result = client.feature_flag(&key).send_event(false).get().unwrap();
        assert_eq!(result.payload.as_ref(), Some(payload));
        assert_eq!(all[&key], result);
        assert_eq!(result.request_id.as_deref(), Some("legacy"));
        assert_eq!(result.id, None);
        assert_eq!(result.version, None);
    }
    assert!(all["bad"].enabled);
    assert_eq!(all["bad"].payload, None);
    client.flush_blocking();
    response.assert_calls(1);
    analytics.assert_calls(0);
}

#[test]
fn remote_empty_variants_are_known_disabled_results() {
    for body in [
        json!({"featureFlags":{"empty":"","disabled":false}}),
        json!({"flags":{
            "empty":{"key":"empty","enabled":true,"variant":""},
            "disabled":{"key":"disabled","enabled":false,"variant":"ignored"}
        }}),
    ] {
        let server = MockServer::start();
        let response = server.mock(|when, then| {
            when.path("/flags/");
            then.status(200).json_body(body);
        });
        let client = Client::init_blocking(config(&server));
        reload(&client);
        let result = client
            .feature_flag("empty")
            .send_event(false)
            .get()
            .unwrap();
        assert!(
            !result.enabled,
            "an empty variant is a known disabled evaluation"
        );
        assert_eq!(result.variant.as_deref(), Some(""));
        let results = client.get_feature_flags().unwrap();
        assert_eq!(results["empty"], result);
        assert!(!results["disabled"].enabled);
        assert_eq!(results["disabled"].variant, None);
        assert_eq!(client.feature_flag("missing").get(), None);
        response.assert_calls(1);
    }
}

#[test]
fn unavailable_empty_disabled_invalid_and_closed_have_explicit_results() {
    let server = MockServer::start();
    let requests = server.mock(|when, then| {
        when.path_not("/array/phc_test/config");
        then.status(500);
    });
    let client = Client::init_blocking(config(&server));
    assert_eq!(client.get_feature_flags(), None);
    // Reads of missing flags report $feature_flag_called; this test checks
    // only the returned values, so it reads silently.
    assert_eq!(client.feature_flag("missing").send_event(false).get(), None);
    for mode in 0..4 {
        let mut config = config(&server);
        bootstrap(&mut config);
        match mode {
            0 => config.bootstrap_feature_flags = Some(HashMap::new()),
            1 => config.disabled = true,
            2 => config.project_token.clear(),
            _ => {}
        }
        let client = Client::init_blocking(config);
        let lookup = client.feature_flag("nullable").send_event(false);
        if mode == 3 {
            client.shutdown_blocking(None);
        }
        assert_eq!(client.get_feature_flags(), (mode == 0).then(HashMap::new));
        assert_eq!(lookup.get(), None);
    }
    requests.assert_calls(0);
}

#[test]
fn partial_and_complete_results_update_rich_exports_as_one_cache() {
    let server = MockServer::start();
    let mut config = config(&server);
    bootstrap(&mut config);
    let client = Client::init_blocking(config);
    let before = client.get_feature_flags().unwrap();
    let mut partial = server.mock(|when, then| {
        when.path("/flags/");
        then.status(200)
            .json_body(json!({"featureFlags":{"variant":false}, "errorsWhileComputingFlags":true}));
    });
    reload(&client);
    partial.assert_calls(1);
    partial.delete();
    let results = client.get_feature_flags().unwrap();
    assert!(!results["variant"].enabled);
    assert_eq!(results["variant"].variant, None);
    assert_eq!(results["variant"].payload, None);
    assert!(results["variant"].partial);
    assert_eq!(results["nullable"], before["nullable"]);
    assert_eq!(results["plain"], before["plain"]);
    let complete = server.mock(|when, then| {
        when.path("/flags/");
        then.status(200).json_body(json!({"featureFlags":{}}));
    });
    reload(&client);
    complete.assert_calls(1);
    assert_eq!(client.get_feature_flags(), Some(HashMap::new()));
    assert_eq!(client.feature_flag("nullable").get(), None);
}

#[test]
fn restored_cache_exports_match_and_survive_a_targeting_change_and_restart() {
    let server = MockServer::start();
    let requests = server.mock(|when, then| {
        when.path_not("/array/phc_test/config");
        then.status(500);
    });
    let directory =
        std::env::temp_dir().join(format!("posthog-flag-reads-{}", uuid::Uuid::now_v7()));
    let make_config = || {
        let mut config = config(&server);
        config.persistence = Persistence::Directory(directory.clone());
        config
    };
    let mut first = make_config();
    bootstrap(&mut first);
    let client = Client::init_blocking(first);
    let expected = client.get_feature_flags();
    assert_eq!(
        client.shutdown_blocking(None).state,
        posthog::stateful::PersistenceOutcome::Persisted
    );
    drop(client);
    let client = Client::init_blocking(make_config());
    assert_eq!(client.get_feature_flags(), expected);
    client.set_person_properties_for_flags(
        HashMap::from([("plan".into(), json!("pro"))]),
        Some(false),
    );
    assert_eq!(client.get_feature_flags(), expected);
    assert_eq!(
        client.shutdown_blocking(None).state,
        posthog::stateful::PersistenceOutcome::Persisted
    );
    drop(client);
    // The other SDKs restore their whole cache, so an offline launch still
    // has flags after a targeting change.
    let client = Client::init_blocking(make_config());
    assert_eq!(client.get_feature_flags(), expected);
    client.shutdown_blocking(None);
    drop(client);
    requests.assert_calls(0);
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(feature = "async-client")]
#[test]
fn runtime_free_async_initialization_exposes_synchronous_rich_reads() {
    let server = MockServer::start();
    let requests = server.mock(|when, then| {
        when.path_not("/array/phc_test/config");
        then.status(500);
    });
    let mut config = config(&server);
    bootstrap(&mut config);
    futures::executor::block_on(async {
        let client = Client::init(config).await;
        let results = client.get_feature_flags().unwrap();
        assert_eq!(results["variant"].variant.as_deref(), Some("blue"));
        assert_eq!(
            client
                .feature_flag("nullable")
                .send_event(false)
                .get()
                .unwrap(),
            results["nullable"]
        );
        assert_eq!(results["nullable"].payload, Some(Value::Null));
        client.shutdown(None).await;
    });
    requests.assert_calls(0);
}

#[test]
fn lookup_shortcuts_read_the_cached_value_and_payload() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.path("/flags/");
        then.status(200).json_body(json!({"flags":{
            "variant":{"key":"variant","enabled":true,"variant":"blue","metadata":{"id":1,"version":1,"payload":"\"123\""}},
            "nullable":{"key":"nullable","enabled":true,"metadata":{"id":2,"version":1,"payload":"null"}},
            "plain":{"key":"plain","enabled":true},
            "disabled":{"key":"disabled","enabled":false},
            "empty":{"key":"empty","enabled":true,"variant":""}
        }}));
    });
    let client = Client::init_blocking(config(&server));
    reload(&client);
    assert!(client.feature_flag("variant").is_enabled());
    assert!(client.feature_flag("plain").is_enabled());
    assert!(!client.feature_flag("disabled").is_enabled());
    assert!(!client.feature_flag("empty").is_enabled());
    assert!(!client.feature_flag("missing").is_enabled());
    assert_eq!(
        client.feature_flag("variant").value(),
        Some(FlagValue::String("blue".into()))
    );
    assert_eq!(
        client.feature_flag("plain").value(),
        Some(FlagValue::Boolean(true))
    );
    assert_eq!(
        client.feature_flag("disabled").value(),
        Some(FlagValue::Boolean(false))
    );
    assert_eq!(
        client.feature_flag("empty").value(),
        Some(FlagValue::Boolean(false))
    );
    assert_eq!(client.feature_flag("missing").value(), None);
    assert_eq!(client.feature_flag("variant").payload(), Some(json!("123")));
    assert_eq!(client.feature_flag("nullable").payload(), Some(Value::Null));
    assert_eq!(client.feature_flag("plain").payload(), None);
    client.shutdown_blocking(None);
    assert!(!client.feature_flag("variant").is_enabled());
    assert_eq!(client.feature_flag("variant").value(), None);
}

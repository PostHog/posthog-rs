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
fn bootstrap_exports_are_owned_memory_only_and_silent_in_all_consent_modes() {
    for opted_out in [false, true] {
        let server = MockServer::start();
        let requests = server.mock(|_, then| {
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
        let expected = HashMap::from([
            ("variant".into(), FlagValue::String("blue".into())),
            ("nullable".into(), FlagValue::Boolean(true)),
            ("plain".into(), FlagValue::Boolean(true)),
        ]);
        for _ in 0..10 {
            assert_eq!(client.get_feature_flags(), Some(expected.clone()));
            assert_eq!(
                client.get_feature_flag_payload("variant"),
                Some(json!("123"))
            );
            assert_eq!(
                client.get_feature_flag_payload("nullable"),
                Some(Value::Null)
            );
            assert_eq!(client.get_feature_flag_payload("plain"), None);
            assert_eq!(client.get_feature_flag_payload("missing"), None);
            assert_eq!(client.get_feature_flag_payload("disabled"), None);
            assert_eq!(
                client.get_feature_flag_payloads(),
                Some(HashMap::from([
                    ("variant".into(), json!("123")),
                    ("nullable".into(), Value::Null)
                ]))
            );
        }
        let mut values = client.get_feature_flags().unwrap();
        values.clear();
        let mut payloads = client.get_feature_flag_payloads().unwrap();
        payloads.insert("variant".into(), json!("changed"));
        assert_eq!(client.get_feature_flags(), Some(expected));
        assert_eq!(
            client.get_feature_flag_payload("variant"),
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
fn v2_exports_preserve_disabled_values_and_decoded_payload_types_without_reparsing() {
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
    client.reload_feature_flags();
    client.flush_blocking();
    let errors_before = errors.load(Ordering::SeqCst);
    assert!(
        errors_before > 0,
        "malformed wire payload is diagnosed during load"
    );
    let values = client.get_feature_flags().unwrap();
    assert_eq!(values["disabled"], FlagValue::Boolean(false));
    assert_eq!(values["bad"], FlagValue::Boolean(true));
    assert!(!values.contains_key("missing"));
    let all_payloads = client.get_feature_flag_payloads().unwrap();
    for (i, payload) in payloads.iter().enumerate() {
        let key = format!("flag-{i}");
        assert_eq!(values[&key], FlagValue::String("blue".into()));
        assert_eq!(client.get_feature_flag_payload(&key), Some(payload.clone()));
        assert_eq!(all_payloads.get(&key), Some(payload));
    }
    assert_eq!(
        client.get_feature_flag_payload("disabled"),
        Some(Value::Null)
    );
    assert_eq!(client.get_feature_flag_payload("bad"), None);
    assert!(!all_payloads.contains_key("bad"));
    client.flush_blocking();
    response.assert_calls(1);
    analytics.assert_calls(0);
    assert_eq!(errors.load(Ordering::SeqCst), errors_before);
    assert_eq!(client.peek_session_id(), None);
}

#[test]
fn unavailable_empty_disabled_invalid_and_closed_have_explicit_results() {
    let server = MockServer::start();
    let requests = server.mock(|_, then| {
        then.status(500);
    });
    let client = Client::init_blocking(config(&server));
    assert_eq!(client.get_feature_flags(), None);
    assert_eq!(client.get_feature_flag_payloads(), None);
    assert_eq!(client.get_feature_flag_payload("missing"), None);
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
        if mode == 3 {
            client.shutdown_blocking(None);
        }
        let expected = (mode == 0).then(HashMap::new);
        assert_eq!(client.get_feature_flags(), expected);
        assert_eq!(
            client.get_feature_flag_payloads(),
            (mode == 0).then(HashMap::new)
        );
        assert_eq!(client.get_feature_flag_payload("nullable"), None);
    }
    requests.assert_calls(0);
}

#[test]
fn partial_and_complete_results_update_value_payload_exports_as_one_cache() {
    let server = MockServer::start();
    let mut config = config(&server);
    bootstrap(&mut config);
    let client = Client::init_blocking(config);
    let mut partial = server.mock(|when, then| {
        when.path("/flags/");
        then.status(200)
            .json_body(json!({"featureFlags":{"variant":false}, "errorsWhileComputingFlags":true}));
    });
    client.reload_feature_flags();
    client.flush_blocking();
    partial.assert_calls(1);
    partial.delete();
    assert_eq!(
        client.get_feature_flags().unwrap()["variant"],
        FlagValue::Boolean(false)
    );
    assert_eq!(client.get_feature_flag_payload("variant"), None);
    assert_eq!(
        client.get_feature_flag_payload("nullable"),
        Some(Value::Null)
    );
    let complete = server.mock(|when, then| {
        when.path("/flags/");
        then.status(200).json_body(json!({"featureFlags":{}}));
    });
    client.reload_feature_flags();
    client.flush_blocking();
    complete.assert_calls(1);
    assert_eq!(client.get_feature_flags(), Some(HashMap::new()));
    assert_eq!(client.get_feature_flag_payloads(), Some(HashMap::new()));
    assert_eq!(client.get_feature_flag_payload("nullable"), None);
}

#[test]
fn restored_cache_exports_match_and_suppressed_targeting_retains_only_live_values() {
    let server = MockServer::start();
    let requests = server.mock(|_, then| {
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
    let expected_values = client.get_feature_flags();
    let expected_payloads = client.get_feature_flag_payloads();
    assert_eq!(
        client.shutdown_blocking(None).state,
        posthog::stateful::PersistenceOutcome::Persisted
    );
    drop(client);
    let client = Client::init_blocking(make_config());
    assert_eq!(client.get_feature_flags(), expected_values);
    assert_eq!(client.get_feature_flag_payloads(), expected_payloads);
    client.set_person_properties_for_flags(
        HashMap::from([("plan".into(), json!("pro"))]),
        Some(false),
    );
    assert_eq!(client.get_feature_flags(), expected_values);
    assert_eq!(client.get_feature_flag_payloads(), expected_payloads);
    assert_eq!(
        client.shutdown_blocking(None).state,
        posthog::stateful::PersistenceOutcome::Persisted
    );
    drop(client);
    let client = Client::init_blocking(make_config());
    assert_eq!(
        client.get_feature_flags(),
        None,
        "restart rejects stale producing context"
    );
    assert_eq!(client.get_feature_flag_payloads(), None);
    client.shutdown_blocking(None);
    drop(client);
    requests.assert_calls(0);
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(feature = "async-client")]
#[test]
fn runtime_free_async_initialization_exposes_synchronous_bootstrap_exports() {
    let server = MockServer::start();
    let requests = server.mock(|_, then| {
        then.status(500);
    });
    let mut config = config(&server);
    bootstrap(&mut config);
    futures::executor::block_on(async {
        let client = Client::init(config).await;
        assert_eq!(
            client.get_feature_flags().unwrap()["variant"],
            FlagValue::String("blue".into())
        );
        assert_eq!(
            client.get_feature_flag_payload("nullable"),
            Some(Value::Null)
        );
        client.shutdown(None).await;
    });
    requests.assert_calls(0);
}

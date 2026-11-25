use super::*;
use crate::FlagValue;

fn bootstrap(config: &mut Config) {
    config.bootstrap_feature_flags = Some(HashMap::from([
        ("change".into(), FlagValue::String("first".into())),
        ("keep".into(), FlagValue::Boolean(true)),
        ("disabled".into(), FlagValue::Boolean(false)),
        ("empty".into(), FlagValue::String(String::new())),
    ]));
    config.bootstrap_feature_flag_payloads = Some(HashMap::from([
        ("change".into(), json!("123")),
        ("keep".into(), Value::Null),
        ("disabled".into(), json!("ignored")),
        ("empty".into(), json!("ignored")),
        ("orphan".into(), json!("ignored")),
    ]));
}

#[test]
fn startup_no_flags_hint_preserves_already_accepted_bootstrap_report() {
    let server = httpmock::MockServer::start();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    server.mock(|when, then| {
        when.path("/array/phc_test/config");
        then.respond_with(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv_timeout(WAIT).unwrap();
            http::Response::builder()
                .status(200)
                .body(r#"{"hasFeatureFlags":false}"#.to_owned())
                .unwrap()
                .into()
        });
    });
    let flags = server.mock(|when, then| {
        when.path("/flags/");
        then.status(200).json_body(json!({"featureFlags":{}}));
    });
    server.mock(|when, then| {
        when.path("/i/v1/analytics/events");
        then.status(200).json_body(json!({"results":{}}));
    });
    let mut config = config(&server.base_url());
    config.preload_feature_flags = true;
    config.before_send.clear();
    config.bootstrap_feature_flags = Some(HashMap::from([("a".into(), FlagValue::Boolean(true))]));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    config.before_send(move |event| {
        seen.fetch_add(1, Ordering::SeqCst);
        Some(event)
    });
    let client = Client::init_blocking(config);
    entered_rx.recv_timeout(WAIT).unwrap();
    assert!(client.feature_flag("a").get().unwrap().enabled);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release_tx.send(()).unwrap();
    client.transport.as_ref().unwrap().request_barrier();
    assert!(client.feature_flag("a").get().unwrap().enabled);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    flags.assert_calls(0);
    client.shutdown_blocking(None);
}

#[test]
fn bootstrap_filters_values_and_keeps_decoded_payloads_before_preload_completes() {
    for partial in [false, true] {
        let server = Server::new(vec![(
            200,
            json!({"featureFlags":{"change":false},
            "errorsWhileComputingFlags":partial, "requestId":"remote"}),
        )]);
        let mut config = config(&server.host);
        config.preload_feature_flags = true;
        config.opt_out = true;
        config.person_profiles = crate::stateful::PersonProfiles::Never;
        bootstrap(&mut config);
        let client = Client::init_blocking(config);
        server.request();
        let before = client.state.as_ref().unwrap().snapshot();
        let cache = before.flags.cache.as_ref().unwrap();
        assert_eq!(cache.flags.len(), 2);
        assert_eq!(
            cache.flags["change"].record.variant.as_deref(),
            Some("first")
        );
        assert_eq!(cache.flags["change"].record.payload, Some(json!("123")));
        assert_eq!(cache.flags["keep"].record.payload, Some(Value::Null));
        assert_eq!(cache.flags["change"].request_id, None);
        assert_eq!(cache.flags["change"].record.id, None);
        assert_eq!(cache.flags["change"].record.version, None);
        assert!(!before.flag_load.remote_received);
        assert!(Arc::ptr_eq(
            before.flag_load.bootstrap.as_ref().unwrap(),
            cache,
        ));
        server.respond();
        client.transport.as_ref().unwrap().request_barrier();
        server.finish();
        let after = client.state.as_ref().unwrap().snapshot();
        let flags = &after.flags.cache.as_ref().unwrap().flags;
        assert!(!flags["change"].record.enabled);
        assert_eq!(flags["change"].record.payload, None);
        assert_eq!(flags["change"].request_id.as_deref(), Some("remote"));
        assert_eq!(flags.contains_key("keep"), partial);
        if partial {
            assert_eq!(flags["keep"].request_id, None);
        }
        assert!(after.flag_load.remote_received);
        assert_eq!(after.flag_load.bootstrap, before.flag_load.bootstrap);
        assert!(client.is_opt_out());
        assert_eq!(client.peek_session_id(), None);
    }
}

#[test]
fn absent_empty_and_configured_bootstrap_have_distinct_restored_cache_behavior() {
    for mode in 0..3 {
        let directory =
            std::env::temp_dir().join(format!("posthog-bootstrap-{}", uuid::Uuid::now_v7()));
        let make_config = || {
            let mut config = config("http://127.0.0.1:1");
            config.persistence = Persistence::Directory(directory.clone());
            config
        };
        let client = Client::init_blocking(make_config());
        seed(&client);
        client.shutdown_blocking(None);
        drop(client);
        let mut next = make_config();
        next.bootstrap_feature_flag_payloads =
            Some(HashMap::from([("old".into(), json!("orphan"))]));
        if mode == 1 {
            next.bootstrap_feature_flags = Some(HashMap::new());
        }
        if mode == 2 {
            bootstrap(&mut next);
        }
        let client = Client::init_blocking(next);
        let state = client.state.as_ref().unwrap().snapshot();
        let flags = &state.flags.cache.as_ref().unwrap().flags;
        assert_eq!(flags.len(), [1, 0, 2][mode]);
        assert_eq!(flags.contains_key("old"), mode == 0);
        if mode == 0 {
            assert_eq!(flags["old"].record.payload, Some(json!("old-payload")));
        }
        assert_eq!(state.flag_load.bootstrap.is_some(), mode != 0);
        client.shutdown_blocking(None);
        drop(client);
        // Cached values may persist; the setup reporting marker and bootstrap
        // context must never be inferred from that cache on a subsequent start.
        let restored = Client::init_blocking(make_config());
        let state = restored.state.as_ref().unwrap().snapshot();
        assert!(state.flag_load.bootstrap.is_none());
        assert!(!state.flag_load.remote_received);
        assert_eq!(
            state.flags.cache.as_ref().unwrap().flags.len(),
            [1, 0, 2][mode]
        );
        restored.shutdown_blocking(None);
        drop(restored);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn bootstrap_uses_final_reconciled_identity_before_preload() {
    let directory =
        std::env::temp_dir().join(format!("posthog-bootstrap-id-{}", uuid::Uuid::now_v7()));
    let server = Server::new(vec![(200, json!({"featureFlags":{}}))]);
    let mut first = config(&server.host);
    first.persistence = Persistence::Directory(directory.clone());
    let client = Client::init_blocking(first);
    client.shutdown_blocking(None);
    drop(client);
    let mut next = config(&server.host);
    next.persistence = Persistence::Directory(directory.clone());
    next.bootstrap_anonymous_id = None;
    next.bootstrap_identified_id = Some("person".into());
    next.opt_out = true;
    next.preload_feature_flags = true;
    bootstrap(&mut next);
    let client = Client::init_blocking(next);
    let request = server.request();
    let state = client.state.as_ref().unwrap().snapshot();
    server.respond();
    client.transport.as_ref().unwrap().request_barrier();
    server.finish();
    assert_eq!(request["distinct_id"], "person");
    assert_eq!(state.flag_load.request, 1);
    let cache = state.flag_load.bootstrap.as_ref().unwrap();
    assert_eq!(cache.context.distinct_id, "person");
    assert_eq!(cache.context.anonymous_id, "device");
    assert_eq!(cache.flags["keep"].context.distinct_id, "person");
    client.shutdown_blocking(None);
    drop(client);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn failed_preload_retains_bootstrap() {
    let server = Server::new(vec![(503, json!({}))]);
    let mut config = config(&server.host);
    config.preload_feature_flags = true;
    bootstrap(&mut config);
    let (tx, rx) = mpsc::channel();
    config.on_error(move |error| {
        tx.send(matches!(error, crate::PostHogError::FeatureFlags(_)))
            .unwrap()
    });
    let client = Client::init_blocking(config);
    server.request();
    server.respond();
    client.transport.as_ref().unwrap().request_barrier();
    server.finish();
    assert!(rx.recv_timeout(WAIT).unwrap());
    let state = client.state.as_ref().unwrap().snapshot();
    assert!(!state.flag_load.remote_received);
    assert_eq!(
        state.flag_load.completed.as_ref().unwrap().error,
        Some(flags::LoadError::Http(503))
    );
    assert_eq!(state.flags.cache.as_ref().unwrap().flags.len(), 2);
    assert!(state.flag_load.bootstrap.is_some());
    assert!(!client.is_disabled());
}

#[test]
fn obsolete_response_does_not_mark_bootstrap_remote_and_reset_drops_all_context() {
    let server = Server::new(vec![
        (200, json!({"featureFlags":{"obsolete":true}})),
        (200, json!({"featureFlags":{"current":true}})),
        (200, json!({"featureFlags":{}})),
    ]);
    let mut config = config(&server.host);
    bootstrap(&mut config);
    config.preload_feature_flags = true;
    let client = Client::init_blocking(config);
    server.request();
    client.identify("person");
    server.respond();
    // The response for the earlier identity is discarded; identify's own
    // request runs next.
    let current = server.request();
    assert_eq!(current["distinct_id"], "person");
    let state = client.state.as_ref().unwrap().snapshot();
    assert!(!state.flag_load.remote_received);
    assert!(state.flag_load.completed.is_none());
    assert!(state
        .flags
        .cache
        .as_ref()
        .unwrap()
        .flags
        .contains_key("keep"));
    server.respond();
    client.transport.as_ref().unwrap().request_barrier();
    let state = client.state.as_ref().unwrap().snapshot();
    assert!(state.flag_load.remote_received);
    assert!(state
        .flags
        .cache
        .as_ref()
        .unwrap()
        .flags
        .contains_key("current"));
    client.reset();
    server.request();
    let state = client.state.as_ref().unwrap().snapshot();
    assert!(state.flag_load.bootstrap.is_none());
    assert!(state.flags.cache.is_none());
    server.respond();
    client.transport.as_ref().unwrap().request_barrier();
    server.finish();
    let state = client.state.as_ref().unwrap().snapshot();
    assert!(state.flag_load.remote_received);
    assert!(state.flag_load.bootstrap.is_none());
    assert!(state.flags.cache.as_ref().unwrap().flags.is_empty());
}

#[test]
fn deeply_nested_bootstrap_is_live_but_cannot_poison_mandatory_persistence() {
    let directory =
        std::env::temp_dir().join(format!("posthog-bootstrap-depth-{}", uuid::Uuid::now_v7()));
    let mut config = config("http://127.0.0.1:1");
    bootstrap(&mut config);
    let mut payload = Value::Null;
    for _ in 0..150 {
        payload = json!({"nested":payload});
    }
    config
        .bootstrap_feature_flag_payloads
        .as_mut()
        .unwrap()
        .insert("keep".into(), payload.clone());
    config.persistence = Persistence::Directory(directory.clone());
    let client = Client::init_blocking(config);
    client.register(HashMap::from([("persisted".into(), json!(true))]));
    client.opt_out();
    let state = client.state.as_ref().unwrap().snapshot();
    assert_eq!(
        state.flags.cache.as_ref().unwrap().flags["keep"]
            .record
            .payload,
        Some(payload)
    );
    assert_eq!(
        client.flush_blocking().state,
        crate::stateful::PersistenceOutcome::Persisted
    );
    assert_eq!(
        client.shutdown_blocking(None).state,
        crate::stateful::PersistenceOutcome::Persisted
    );
    drop(client);
    let mut next = super::config("http://127.0.0.1:1");
    next.persistence = Persistence::Directory(directory.clone());
    let client = Client::init_blocking(next);
    let state = client.state.as_ref().unwrap().snapshot();
    assert_eq!(state.registered.get("persisted"), Some(&json!(true)));
    assert!(client.is_opt_out());
    assert!(state.flags.cache.is_none());
    assert!(state.flag_load.bootstrap.is_none());
    client.shutdown_blocking(None);
    drop(client);
    std::fs::remove_dir_all(directory).unwrap();
}

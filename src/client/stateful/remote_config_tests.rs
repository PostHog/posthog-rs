use super::*;
use crate::stateful::{Config, Persistence};
use httpmock::prelude::*;
use serde_json::json;

fn config(server: &MockServer, preload: bool) -> Config {
    let mut config = Config::new("example-project");
    config.host = server.base_url();
    config.persistence = Persistence::Memory;
    config.preload_feature_flags = preload;
    config.request_timeout_seconds = 2;
    config
}

#[test]
fn no_flags_hint_preserves_bootstrap_after_suppressed_property_changes() {
    for group_properties in [false, true] {
        let server = MockServer::start();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Mutex::new(release_rx);
        let project = server.mock(|when, then| {
            when.method(GET).path("/array/example-project/config");
            then.respond_with(move |_| {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                http::Response::builder()
                    .status(200)
                    .body(r#"{"hasFeatureFlags":false}"#.to_owned())
                    .unwrap()
                    .into()
            });
        });
        let flags = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(json!({"featureFlags":{}}));
        });
        let mut options = config(&server, true);
        options.request_timeout_seconds = 10;
        options.bootstrap_feature_flags = Some(HashMap::from([(
            "bootstrap".into(),
            crate::FlagValue::Boolean(true),
        )]));
        let client = Client::init_blocking(options);
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let original = client
            .state
            .as_ref()
            .unwrap()
            .snapshot()
            .flags
            .cache
            .clone()
            .unwrap();
        let properties = HashMap::from([("plan".into(), json!("pro"))]);
        if group_properties {
            client.set_group_properties_for_flags("organization", properties, Some(false));
        } else {
            client.set_person_properties_for_flags(properties, Some(false));
        }
        release_tx.send(()).unwrap();
        client.flush_blocking();
        project.assert_calls(1);
        flags.assert_calls(0);
        assert_eq!(
            client
                .state
                .as_ref()
                .unwrap()
                .snapshot()
                .flags
                .cache
                .as_ref(),
            Some(&original)
        );
        assert!(
            client
                .feature_flag("bootstrap")
                .send_event(false)
                .get()
                .unwrap()
                .enabled
        );
        client.shutdown_blocking(None);
    }
}

#[test]
fn project_cache_capacity_failure_reports_without_discarding_live_or_mandatory_state() {
    let root =
        std::env::temp_dir().join(format!("posthog-config-capacity-{}", uuid::Uuid::now_v7()));
    let server = MockServer::start();
    let mut project = server.mock(|when, then| {
        when.method(GET).path("/array/example-project/config");
        then.status(200)
            .json_body(json!({"sessionRecording":{"futureSetting":"x".repeat(4096)}}));
    });
    let (diagnostic_tx, diagnostic_rx) = std::sync::mpsc::channel();
    let settings = || {
        let mut options = config(&server, false);
        options.persistence = Persistence::Directory(root.clone());
        options.bootstrap_anonymous_id = Some("cache-device".into());
        options.max_state_bytes = 1024;
        let diagnostic_tx = diagnostic_tx.clone();
        options.on_error(move |failure| {
            if let crate::PostHogError::Storage(failure) = failure {
                diagnostic_tx
                    .send((
                        failure.operation(),
                        failure.error().kind(),
                        failure.error().to_string(),
                    ))
                    .unwrap();
            }
        });
        options
    };
    let client = Client::init_blocking(settings());
    client.register(HashMap::from([("kept".into(), json!(true))]));
    client.flush_blocking();
    let report = client.flush_blocking();
    assert_eq!(report.state, crate::stateful::PersistenceOutcome::Persisted);
    assert!(client
        .state
        .as_ref()
        .unwrap()
        .snapshot()
        .project_config
        .is_some());
    let diagnostic = diagnostic_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        diagnostic,
        (
            crate::StorageOperation::Write,
            std::io::ErrorKind::InvalidInput,
            "Project configuration cache exceeds state storage bounds; live settings retained"
                .into(),
        )
    );
    let scope = super::super::storage::namespace(&server.base_url(), "example-project", "default");
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join(scope).join("state.json")).unwrap())
            .unwrap();
    assert_eq!(stored["state"]["project_config"], json!(null));
    assert_eq!(stored["state"]["distinct_id"], "cache-device");
    assert_eq!(stored["state"]["registered"]["kept"], true);
    client.shutdown_blocking(None);
    drop(client);
    project.delete();
    let _offline = server.mock(|when, then| {
        when.method(GET).path("/array/example-project/config");
        then.status(503);
    });
    let restored = Client::init_blocking(settings());
    restored.flush_blocking();
    assert_eq!(restored.get_distinct_id(), "cache-device");
    assert_eq!(
        restored.state.as_ref().unwrap().snapshot().registered["kept"],
        true
    );
    assert!(restored
        .state
        .as_ref()
        .unwrap()
        .snapshot()
        .project_config
        .is_none());
    restored.shutdown_blocking(None);
    drop(restored);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn startup_gating_preserves_explicit_reload_and_config_is_bodyless() {
    for (preload, status, body, automatic) in [
        (false, 200, r#"{"hasFeatureFlags":true}"#, 0),
        (false, 503, "unavailable", 0),
        (true, 200, r#"{"hasFeatureFlags":false}"#, 0),
        (true, 200, r#"{"hasFeatureFlags":true}"#, 1),
        (true, 200, r#"{}"#, 1),
        (true, 200, r#"{"hasFeatureFlags":"false"}"#, 1),
        (true, 503, "unavailable", 1),
        (true, 200, "broken", 1),
    ] {
        let server = MockServer::start();
        let project = server.mock(|when, then| {
            when.method(GET)
                .path("/array/example-project/config")
                .header_missing("authorization")
                .header_missing("content-length")
                .header_missing("transfer-encoding");
            then.status(status).body(body);
        });
        let flags = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200)
                .json_body(json!({"featureFlags":{"available":true}}));
        });
        let client = Client::init_blocking(config(&server, preload));
        client.flush_blocking();
        project.assert_calls(1);
        flags.assert_calls(automatic);
        client.reload_feature_flags();
        client.flush_blocking();
        flags.assert_calls(automatic + 1);
        assert!(client.get_feature_flags().unwrap()["available"].enabled);
        client.shutdown_blocking(None);
    }
}

#[test]
fn failed_refresh_keeps_config_but_does_not_apply_a_cached_false_preload_hint() {
    let server = MockServer::start();
    let mut project = server.mock(|when, then| {
        when.method(GET).path("/array/example-project/config");
        then.status(200)
            .json_body(json!({"hasFeatureFlags":false,"sessionRecording":{"endpoint":"/s/"}}));
    });
    let flags = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({"featureFlags":{}}));
    });
    let client = Client::init_blocking(config(&server, false));
    client.flush_blocking();
    let cached = client
        .state
        .as_ref()
        .unwrap()
        .snapshot()
        .project_config
        .clone();
    assert!(cached.is_some());
    project.delete();
    let _failure = server.mock(|when, then| {
        when.method(GET).path("/array/example-project/config");
        then.status(503);
    });
    client.load_remote_config(true);
    client.flush_blocking();
    flags.assert_calls(1);
    assert_eq!(
        client.state.as_ref().unwrap().snapshot().project_config,
        cached
    );
    client.shutdown_blocking(None);
}

#[test]
fn successful_missing_or_malformed_replay_fields_replace_prior_enablement() {
    for response in [
        json!({}),
        json!({"sessionRecording":null}),
        json!({"sessionRecording":42}),
        json!({"sessionRecording":false}),
    ] {
        let server = MockServer::start();
        let mut project = server.mock(|when, then| {
            when.method(GET).path("/array/example-project/config");
            then.status(200).json_body(json!({"sessionRecording":{}}));
        });
        let client = Client::init_blocking(config(&server, false));
        client.flush_blocking();
        assert_eq!(
            client
                .state
                .as_ref()
                .unwrap()
                .snapshot()
                .project_config
                .as_ref()
                .unwrap()
                .session_recording,
            Some(json!({}))
        );
        project.delete();
        let _replacement = server.mock(|when, then| {
            when.method(GET).path("/array/example-project/config");
            then.status(200).json_body(response.clone());
        });
        client.load_remote_config(false);
        client.flush_blocking();
        let expected = response
            .get("sessionRecording")
            .filter(|value| value.is_boolean())
            .cloned();
        assert_eq!(
            client
                .state
                .as_ref()
                .unwrap()
                .snapshot()
                .project_config
                .as_ref()
                .unwrap()
                .session_recording,
            expected
        );
        client.shutdown_blocking(None);
    }
}

#[test]
fn no_flags_hint_preserves_bootstrap_values_and_config_survives_reset() {
    let server = MockServer::start();
    let project = server.mock(|when, then| {
        when.method(GET).path("/array/example-project/config");
        then.status(200)
            .json_body(json!({"hasFeatureFlags":false,"sessionRecording":{}}));
    });
    let flags = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({"featureFlags":{}}));
    });
    let mut options = config(&server, true);
    options.bootstrap_feature_flags = Some(HashMap::from([(
        "bootstrap".into(),
        crate::FlagValue::Boolean(true),
    )]));
    let client = Client::init_blocking(options);
    client.flush_blocking();
    project.assert_calls(1);
    flags.assert_calls(0);
    assert!(client.get_feature_flags().unwrap()["bootstrap"].enabled);
    let cached = client
        .state
        .as_ref()
        .unwrap()
        .snapshot()
        .project_config
        .clone();
    client.reset();
    client.flush_blocking();
    flags.assert_calls(1);
    assert_eq!(
        client.state.as_ref().unwrap().snapshot().project_config,
        cached
    );
    client.shutdown_blocking(None);
}

#[test]
fn project_config_restores_from_the_configured_state_store() {
    let root = std::env::temp_dir().join(format!("posthog-config-{}", uuid::Uuid::now_v7()));
    let server = MockServer::start();
    let mut project = server.mock(|when, then| {
        when.method(GET).path("/array/example-project/config");
        then.status(200).json_body(json!({"sessionRecording":{}}));
    });
    let settings = || {
        let mut config = config(&server, false);
        config.persistence = Persistence::Directory(root.clone());
        config
    };
    let client = Client::init_blocking(settings());
    client.flush_blocking();
    // Config may complete after the flush captured its state revision.
    client.flush_blocking();
    let cached = client
        .state
        .as_ref()
        .unwrap()
        .snapshot()
        .project_config
        .clone();
    assert!(cached.is_some());
    client.shutdown_blocking(None);
    drop(client);
    project.delete();
    let _offline = server.mock(|when, then| {
        when.method(GET).path("/array/example-project/config");
        then.status(503);
    });
    let restored = Client::init_blocking(settings());
    assert_eq!(
        restored.state.as_ref().unwrap().snapshot().project_config,
        cached
    );
    restored.flush_blocking();
    assert_eq!(
        restored.state.as_ref().unwrap().snapshot().project_config,
        cached
    );
    restored.shutdown_blocking(None);
    drop(restored);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn initialization_is_async_and_pending_config_survives_reset_without_duplicate_flags() {
    for has_flags in [false, true] {
        let server = MockServer::start();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Mutex::new(release_rx);
        let project = server.mock(|when, then| {
            when.method(GET).path("/array/example-project/config");
            then.respond_with(move |_| {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                http::Response::builder()
                    .status(200)
                    .body(json!({"hasFeatureFlags":has_flags,"sessionRecording":{}}).to_string())
                    .unwrap()
                    .into()
            });
        });
        let mut options = config(&server, true);
        options.request_timeout_seconds = 10;
        let client = Client::init_blocking(options);
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        client.load_remote_config(true);
        client.reset();
        let distinct_id = client.get_distinct_id();
        let flags = server.mock(|when, then| {
            when.method(POST)
                .path("/flags/")
                .json_body_includes(json!({"distinct_id":distinct_id}).to_string());
            then.status(200)
                .json_body(json!({"featureFlags":{"current-user":true}}));
        });
        release_tx.send(()).unwrap();
        client.flush_blocking();
        project.assert_calls(1);
        flags.assert_calls(1);
        assert!(client.get_feature_flags().unwrap()["current-user"].enabled);
        assert!(client
            .state
            .as_ref()
            .unwrap()
            .snapshot()
            .project_config
            .is_some());
        client.shutdown_blocking(None);
    }
}

#[test]
fn shutdown_during_config_does_not_start_flags_or_install_late_settings() {
    let server = MockServer::start();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = std::sync::Mutex::new(release_rx);
    let project = server.mock(|when, then| {
        when.method(GET).path("/array/example-project/config");
        then.respond_with(move |_| {
            entered_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            http::Response::builder()
                .status(200)
                .body(r#"{"hasFeatureFlags":true,"sessionRecording":{}}"#.to_owned())
                .unwrap()
                .into()
        });
    });
    let flags = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({"featureFlags":{}}));
    });
    let mut options = config(&server, true);
    options.request_timeout_seconds = 10;
    let client = Arc::new(Client::init_blocking(options));
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let closing = client.clone();
    let worker = std::thread::spawn(move || closing.shutdown_blocking(None));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !client.closed.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    let closed = client.closed.load(Ordering::Acquire);
    release_tx.send(()).unwrap();
    let report = worker.join().unwrap();
    assert!(closed);
    assert_eq!(report.delivery, crate::stateful::DeliveryOutcome::Stopped);
    project.assert_calls(1);
    flags.assert_calls(0);
    assert!(client
        .state
        .as_ref()
        .unwrap()
        .snapshot()
        .project_config
        .is_none());
}

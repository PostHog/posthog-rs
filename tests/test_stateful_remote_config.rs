#![cfg(feature = "stateful")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use httpmock::prelude::*;
use posthog::stateful::{CaptureOutcome, Client, Config, DeliveryOutcome, Persistence};
use posthog::FlagValue;
use serde_json::json;

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("posthog-remote-config-{}", uuid::Uuid::now_v7())))
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn settings(server: &MockServer, root: &Directory, preload: bool) -> Config {
    let mut config = Config::new("example-project");
    config.host = server.base_url();
    config.persistence = Persistence::Directory(root.0.clone());
    config.preload_feature_flags = preload;
    config.request_timeout_seconds = 10;
    config.flush_interval = Duration::from_secs(3600);
    config
}

#[test]
fn oversized_config_retains_fitting_flags_on_restart() {
    for oversized in [false, true] {
        let root = Directory::new();
        let server = MockServer::start();

        let mut project = server.mock(|when, then| {
            when.method(GET).path("/array/example-project/config");
            then.status(200).json_body(json!({
                "hasFeatureFlags": true,
                "sessionRecording": {
                    "futureSetting": "x".repeat(if oversized { 8192 } else { 8 })
                }
            }));
        });
        let mut flags = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(json!({
                "featureFlags": {"kept": true}
            }));
        });

        let mut config = settings(&server, &root, true);
        config.max_state_bytes = 4096;
        let client = Client::init_blocking(config);

        // First flush waits for delivery-side config/flags completion.
        // Second captures and drains the resulting state revision.
        client.flush_blocking();
        client.flush_blocking();
        assert!(client.get_feature_flags().unwrap()["kept"].enabled);
        client.shutdown_blocking(None);
        drop(client);

        project.delete();
        flags.delete();
        let _offline = server.mock(|_, then| {
            then.status(503);
        });

        let mut config = settings(&server, &root, false);
        config.max_state_bytes = 4096;
        let restored = Client::init_blocking(config);
        let flags = restored.get_feature_flags();
        restored.shutdown_blocking(None);
        drop(restored);

        assert!(
            flags
                .as_ref()
                .is_some_and(|flags| { flags.get("kept").is_some_and(|flag| flag.enabled) }),
            "oversized={}; restored={:?}",
            oversized,
            flags
        );
    }
}

// A ready-listener's captures are destroyed after request_shutdown has
// requested closure of both workers, not merely after Client.closed changes.
struct CloseNotice(mpsc::Sender<()>);

impl Drop for CloseNotice {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

#[test]
fn shutdown_does_not_start_config_queued_behind_recovery() {
    const WAIT: Duration = Duration::from_secs(15);

    let root = Directory::new();
    let server = MockServer::start();
    let mut first_project = server.mock(|when, then| {
        when.method(GET).path("/array/example-project/config");
        then.status(200).json_body(json!({"hasFeatureFlags": true}));
    });

    // Seed one persisted event without sending it, following the existing
    // zero_shutdown_override_preserves_persisted_events_for_restart test.
    let client = Client::init_blocking(settings(&server, &root, false));
    client.flush_blocking();
    assert!(matches!(
        client.capture("persisted-before-restart"),
        CaptureOutcome::Accepted { .. }
    ));
    client.shutdown_blocking(Some(Duration::ZERO));
    drop(client);

    let scope = std::fs::read_dir(&root.0)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let records = std::fs::read_dir(scope.join("events"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .count();
    assert_eq!(records, 1);

    first_project.delete();
    let project = server.mock(|when, then| {
        when.method(GET).path("/array/example-project/config");
        then.status(200).json_body(json!({"hasFeatureFlags": true}));
    });

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let recovered = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.respond_with(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv_timeout(WAIT).unwrap();
            http::Response::builder()
                .status(200)
                .body(String::new())
                .unwrap()
                .into()
        });
    });

    let (closed_tx, closed_rx) = mpsc::channel();
    let notice = CloseNotice(closed_tx);
    let entered_rx = Mutex::new(entered_rx);

    let mut config = settings(&server, &root, false);
    config.bootstrap_feature_flags = Some(HashMap::from([(
        "ready-gate".into(),
        FlagValue::Boolean(true),
    )]));
    config.on_feature_flags(move |_| {
        let _keep_until_listener_is_destroyed = &notice;
        // notify_ready runs before load_remote_config. Do not allow init
        // to queue config until startup recovery is already blocked.
        entered_rx.lock().unwrap().recv_timeout(WAIT).unwrap();
    });

    let client = Arc::new(Client::init_blocking(config));
    let closing = client.clone();
    let shutdown = std::thread::spawn(move || closing.shutdown_blocking(Some(Duration::ZERO)));

    let closed = closed_rx.recv_timeout(WAIT);
    // Release controlled I/O before any assertion can unwind and join.
    release_tx.send(()).unwrap();
    let report = shutdown.join().unwrap();

    closed.unwrap();
    assert_eq!(report.delivery, DeliveryOutcome::Stopped);
    recovered.assert_calls(1);
    project.assert_calls(0);
}

#[test]
fn false_hint_clears_restored_server_flags_and_notifies_current_and_late_subscribers() {
    let root = Directory::new();
    let server = MockServer::start();
    let mut project = server.mock(|when, then| {
        when.method(GET).path("/array/example-project/config");
        then.status(200).json_body(json!({"hasFeatureFlags": true}));
    });
    let mut flags = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200)
            .json_body(json!({"featureFlags": {"stale": true}}));
    });
    let client = Client::init_blocking(settings(&server, &root, true));
    client.flush_blocking();
    client.flush_blocking();
    assert!(client.get_feature_flags().unwrap()["stale"].enabled);
    client.shutdown_blocking(None);
    drop(client);
    project.delete();
    flags.delete();

    let project = server.mock(|when, then| {
        when.method(GET).path("/array/example-project/config");
        then.status(200)
            .json_body(json!({"hasFeatureFlags": false}));
    });
    let flags = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({"featureFlags": {}}));
    });
    let (updates_tx, updates_rx) = mpsc::channel();
    let mut config = settings(&server, &root, true);
    config.on_feature_flags(move |update| {
        updates_tx.send(update.clone()).unwrap();
    });
    let client = Client::init_blocking(config);
    client.flush_blocking();
    assert!(client.get_feature_flags().unwrap().is_empty());
    let updates: Vec<_> = updates_rx.try_iter().collect();
    assert!(updates.iter().any(|update| {
        update
            .flags
            .as_ref()
            .is_some_and(|flags| flags.contains_key("stale"))
    }));
    let completed = updates.last().unwrap();
    assert!(completed.flags.as_ref().unwrap().is_empty());
    assert!(!completed.errors_loading);
    assert_eq!(completed.error, None);

    let (late_tx, late_rx) = mpsc::channel();
    let _subscription = client.on_feature_flags(move |update| {
        late_tx.send(update.clone()).unwrap();
    });
    let late = late_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(late, *completed);
    project.assert_calls(1);
    flags.assert_calls(0);
    client.shutdown_blocking(None);
}

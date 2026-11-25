#![cfg(feature = "stateful")]

use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use httpmock::prelude::*;
use posthog::stateful::{
    CaptureOutcome, Client, Config, DeliveryOutcome, FeatureFlagsUpdate, Persistence,
    PersistenceOutcome,
};
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
    config.request_timeout = std::time::Duration::from_secs(10);
    config.flush_interval = Duration::from_secs(3600);
    config
}

fn updates_until(
    receiver: &mpsc::Receiver<FeatureFlagsUpdate>,
    complete: impl Fn(&FeatureFlagsUpdate) -> bool,
) -> Vec<FeatureFlagsUpdate> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut updates = Vec::new();
    loop {
        let update = receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("feature flag completion did not arrive before deadline");
        let done = complete(&update);
        updates.push(update);
        if done {
            return updates;
        }
    }
}

#[test]
fn config_completes_while_durable_recovery_is_blocked_and_shutdown_is_bounded() {
    let root = Directory::new();
    let server = MockServer::start();
    let mut first_project = server.mock(|when, then| {
        when.method(GET).path("/array/example-project/config");
        then.status(200).json_body(json!({"hasFeatureFlags": true}));
    });
    let mut retry = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(503).header("Retry-After", "3600");
    });
    let client = Client::init_blocking(settings(&server, &root, false));
    let identity = client.get_distinct_id();
    assert!(matches!(
        client.capture("persisted-before-restart"),
        CaptureOutcome::Accepted { .. }
    ));
    client.opt_out();
    let flushed = client.flush_blocking();
    assert_eq!(flushed.state, PersistenceOutcome::Persisted);
    assert_eq!(flushed.delivery, DeliveryOutcome::Attempted);
    // A budget shorter than the retry wait stops without resending.
    assert_eq!(
        client
            .shutdown_blocking(Some(Duration::from_secs(1)))
            .delivery,
        DeliveryOutcome::Stopped
    );
    drop(client);
    retry.assert_calls(1);
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
    retry.delete();

    // Each accepted connection needs independent progress while the upload is held.
    // httpmock's local server runs synchronous response callbacks on one runtime thread.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let host = format!("http://{}", listener.local_addr().unwrap());
    let accepting = listener.try_clone().unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let (returned_tx, returned_rx) = mpsc::channel();
    let (release_config, config_gate) = tokio::sync::oneshot::channel();
    let serving = std::thread::spawn(move || {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let listener = tokio::net::TcpListener::from_std(accepting).unwrap();
                let mut upload_gate = Some(release_rx);
                let mut config_gate = Some(config_gate);
                let mut handlers = Vec::new();
                for _ in 0..2 {
                    let (stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                        .await.unwrap().unwrap();
                    let mut stream = BufReader::new(stream);
                    let mut line = String::new();
                    let mut length = 0;
                    let route = tokio::time::timeout(Duration::from_secs(5), async {
                        stream.read_line(&mut line).await.unwrap();
                        let route = line.trim_end().to_owned();
                        loop {
                            line.clear();
                            assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                            if line == "\r\n" {
                                break;
                            }
                            if let Some((name, value)) = line.split_once(':') {
                                if name.eq_ignore_ascii_case("content-length") {
                                    length = value.trim().parse().unwrap();
                                }
                            }
                        }
                        stream.read_exact(&mut vec![0; length]).await.unwrap();
                        route
                    }).await.unwrap();
                    match route.as_str() {
                        "POST /i/v1/analytics/events HTTP/1.1" => {
                            let gate = upload_gate.take().expect("one recovered upload");
                            let entered = entered_tx.clone();
                            let returned = returned_tx.clone();
                            handlers.push(tokio::spawn(async move {
                                assert!(length > 0);
                                entered.send(()).unwrap();
                                tokio::time::timeout(Duration::from_secs(10), gate).await.unwrap().unwrap();
                                returned.send(()).unwrap();
                                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                            }));
                        }
                        "GET /array/example-project/config HTTP/1.1" => {
                            let gate = config_gate.take().expect("one config request");
                            handlers.push(tokio::spawn(async move {
                                assert_eq!(length, 0);
                                tokio::time::timeout(Duration::from_secs(5), gate).await.unwrap().unwrap();
                                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 25\r\nConnection: close\r\n\r\n{\"hasFeatureFlags\":false}").await.unwrap();
                            }));
                        }
                        _ => panic!("unexpected request: {}", route),
                    }
                }
                for handler in handlers {
                    handler.await.unwrap();
                }
            });
    });
    let (completed, updates) = mpsc::channel();
    let mut config = settings(&server, &root, false);
    config.host = host;
    config.request_timeout = std::time::Duration::from_secs(30);
    config.on_feature_flags(move |update| {
        completed.send(update.clone()).unwrap();
    });
    let client = Arc::new(Client::init_blocking(config));
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    release_config.send(()).unwrap();
    updates_until(&updates, |update| {
        !update.errors_loading && update.flags.as_ref().is_some_and(|flags| flags.is_empty())
    });
    assert!(returned_rx.try_recv().is_err());
    assert_eq!(client.get_distinct_id(), identity);
    assert!(client.is_opt_out());
    assert_eq!(
        client.capture("new opted-out event"),
        CaptureOutcome::OptedOut
    );

    let closing = client.clone();
    let (closed_tx, closed_rx) = mpsc::channel();
    let shutdown = std::thread::spawn(move || {
        let start = Instant::now();
        let report = closing.shutdown_blocking(Some(Duration::from_millis(20)));
        closed_tx.send((report, start.elapsed())).unwrap();
    });
    let (report, elapsed) = closed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(report.delivery, DeliveryOutcome::TimedOut);
    assert!(elapsed < Duration::from_secs(1));
    assert!(returned_rx.try_recv().is_err());
    assert_eq!(client.capture("closed"), CaptureOutcome::Closed);
    shutdown.join().unwrap();
    release_tx.send(()).unwrap();
    returned_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.shutdown_blocking(None).delivery != DeliveryOutcome::Stopped {
        assert!(
            Instant::now() < deadline,
            "delivery worker did not finish after release"
        );
        std::thread::yield_now();
    }
    serving.join().unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
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
    let (seed_tx, seed_rx) = mpsc::channel();
    let mut config = settings(&server, &root, true);
    config.on_feature_flags(move |update| {
        seed_tx.send(update.clone()).unwrap();
    });
    let client = Client::init_blocking(config);
    updates_until(&seed_rx, |update| {
        !update.errors_loading
            && update
                .flags
                .as_ref()
                .is_some_and(|flags| flags.contains_key("stale"))
    });
    assert_eq!(client.flush_blocking().state, PersistenceOutcome::Persisted);
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
    let updates = updates_until(&updates_rx, |update| {
        !update.errors_loading && update.flags.as_ref().is_some_and(|flags| flags.is_empty())
    });
    assert!(client.get_feature_flags().unwrap().is_empty());
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

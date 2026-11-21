#![cfg(feature = "stateful")]

use std::path::{Path, PathBuf};
#[cfg(feature = "async-client")]
use std::sync::Arc;
use std::time::Duration;

use httpmock::{Method::POST, MockServer};

mod stateful_support;
use posthog::stateful::{
    CaptureOutcome, Client, Config, DeliveryOutcome, Persistence, PersistenceOutcome,
};
use stateful_support::accept_all;

fn config(server: &MockServer) -> Config {
    let mut config = Config::new("phc_test");
    config.preload_feature_flags = false;
    config.host = server.base_url();
    config.persistence = Persistence::Memory;
    config.flush_interval = Duration::from_secs(3600);
    config
}

#[test]
fn blocking_shutdown_override_can_extend_the_configured_drain_budget() {
    let server = MockServer::start();
    let batch = server.mock(|when, then| {
        when.method(POST);
        then.respond_with(accept_all);
    });
    let mut config = config(&server);
    config.shutdown_timeout = Duration::ZERO;
    let client = Client::init_blocking(config);
    assert!(matches!(
        client.capture("queued"),
        CaptureOutcome::Accepted { .. }
    ));
    let report = client.shutdown_blocking(Some(Duration::from_secs(5)));
    assert_eq!(report.delivery, DeliveryOutcome::Stopped);
    assert_eq!(report.state, PersistenceOutcome::MemoryOnly);
    assert_eq!(client.capture("closed"), CaptureOutcome::Closed);
    assert_eq!(client.shutdown_blocking(None), report);
    batch.assert_calls(1);
}

#[test]
fn zero_shutdown_override_preserves_persisted_events_for_restart() {
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let directory =
        Directory(std::env::temp_dir().join(format!("posthog-drains-{}", uuid::Uuid::now_v7())));
    let server = MockServer::start();
    let mut rejected = server.mock(|when, then| {
        when.method(POST);
        then.status(400);
    });
    let (entered, waiting) = std::sync::mpsc::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let gate = std::sync::Mutex::new(gate);
    let mut setup = config(&server);
    setup.persistence = Persistence::Directory(directory.0.clone());
    setup.flush_at = 1;
    setup.on_error(move |failure| {
        if matches!(failure, posthog::PostHogError::Capture(_)) {
            entered.send(()).unwrap();
            gate.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    });
    let client = Client::init_blocking(setup);
    client.flush_blocking();
    let identity = client.get_distinct_id();
    assert!(matches!(
        client.capture("persisted"),
        CaptureOutcome::Accepted { .. }
    ));
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    let start = std::time::Instant::now();
    assert_eq!(
        client.shutdown_blocking(Some(Duration::ZERO)).delivery,
        DeliveryOutcome::TimedOut
    );
    drop(client);
    assert!(start.elapsed() < Duration::from_secs(1));
    release.send(()).unwrap();
    rejected.delete();
    let scope = std::fs::read_dir(&directory.0)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    // Reopening requires actual storage ownership release, not merely timeout.
    let lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scope.join("lock"))
        .unwrap();
    let end = std::time::Instant::now() + Duration::from_secs(5);
    while fs2::FileExt::try_lock_exclusive(&lease).is_err() {
        assert!(
            std::time::Instant::now() < end,
            "storage owner did not finish"
        );
        std::thread::yield_now();
    }
    fs2::FileExt::unlock(&lease).unwrap();
    let batch = server.mock(|when, then| {
        when.method(POST);
        then.respond_with(accept_all);
    });
    let records = || {
        std::fs::read_dir(scope.join("events"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .count()
    };
    assert_eq!(records(), 1);
    let mut setup = config(&server);
    setup.persistence = Persistence::Directory(directory.0.clone());
    let restored = Client::init_blocking(setup);
    assert_eq!(restored.get_distinct_id(), identity);
    restored.flush_blocking();
    restored.shutdown_blocking(None);
    batch.assert_calls(1);
    assert_eq!(records(), 0);
}

#[cfg(feature = "async-client")]
#[test]
fn async_drains_are_runtime_independent_and_match_blocking_reports() {
    futures::executor::block_on(async {
        let server = MockServer::start();
        let batch = server.mock(|when, then| {
            when.method(POST);
            then.respond_with(accept_all);
        });
        let mut setup = config(&server);
        setup.shutdown_timeout = Duration::ZERO;
        let client = Client::init(setup).await;
        assert!(matches!(
            client.capture("flushed"),
            CaptureOutcome::Accepted { .. }
        ));
        let flushed = client.flush().await;
        assert_eq!(flushed.state, PersistenceOutcome::MemoryOnly);
        assert_eq!(flushed.delivery, DeliveryOutcome::Attempted);
        assert!(matches!(
            client.capture("shutdown"),
            CaptureOutcome::Accepted { .. }
        ));
        let stopped = client.shutdown(Some(Duration::from_secs(5))).await;
        assert_eq!(stopped.state, PersistenceOutcome::MemoryOnly);
        assert_eq!(stopped.delivery, DeliveryOutcome::Stopped);
        assert_eq!(client.shutdown(Some(Duration::MAX)).await, stopped);
        assert_eq!(client.shutdown_blocking(None), stopped);
        assert_eq!(client.flush().await.delivery, DeliveryOutcome::Closed);
        assert_eq!(client.capture("closed"), CaptureOutcome::Closed);
        batch.assert_calls(2);
        let disabled = Client::init(Config::new("")).await;
        assert_eq!(disabled.flush().await.delivery, DeliveryOutcome::Disabled);
        assert_eq!(
            disabled.shutdown(None).await.state,
            PersistenceOutcome::Disabled
        );
        fn assert_send(_: impl Send) {}
        assert_send(client.flush());
        assert_send(client.shutdown(None));
    });
}

#[cfg(feature = "async-client")]
#[test]
fn async_drains_from_delivery_callback_defer_without_waiting() {
    use std::sync::{Mutex, Weak};
    let server = MockServer::start();
    let rejected = server.mock(|when, then| {
        when.method(POST);
        then.status(400);
    });
    let slot = Arc::new(Mutex::new(Weak::<Client>::new()));
    let callback_slot = slot.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut setup = config(&server);
    setup.on_error(move |failure| {
        if !matches!(failure, posthog::PostHogError::Capture(_)) {
            return;
        }
        let client = callback_slot.lock().unwrap().upgrade().unwrap();
        let flush = futures::executor::block_on(client.flush());
        let shutdown = futures::executor::block_on(client.shutdown(Some(Duration::ZERO)));
        tx.send((flush, shutdown)).unwrap();
    });
    let client = Arc::new(Client::init_blocking(setup));
    *slot.lock().unwrap() = Arc::downgrade(&client);
    client.capture("rejected");
    futures::executor::block_on(client.flush());
    let (flush, shutdown) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(flush.delivery, DeliveryOutcome::Deferred);
    assert_eq!(shutdown.state, PersistenceOutcome::Deferred);
    assert!(matches!(
        futures::executor::block_on(client.shutdown(None)).delivery,
        DeliveryOutcome::Stopped | DeliveryOutcome::TimedOut
    ));
    rejected.assert_calls(1);
}

#[cfg(feature = "async-client")]
#[test]
fn async_hook_shutdown_prevents_outer_capture_admission() {
    use std::sync::{Mutex, Weak};
    let server = MockServer::start();
    let slot = Arc::new(Mutex::new(Weak::<Client>::new()));
    let hook_slot = slot.clone();
    let mut setup = config(&server);
    setup.before_send(move |event| {
        let client = hook_slot.lock().unwrap().upgrade().unwrap();
        assert_eq!(
            futures::executor::block_on(client.flush()).delivery,
            DeliveryOutcome::Deferred
        );
        assert_eq!(
            futures::executor::block_on(client.shutdown(None)).delivery,
            DeliveryOutcome::Deferred
        );
        Some(event)
    });
    let client = Arc::new(Client::init_blocking(setup));
    *slot.lock().unwrap() = Arc::downgrade(&client);
    assert_eq!(client.capture("outer"), CaptureOutcome::Closed);
    futures::executor::block_on(client.shutdown(None));
}

#[cfg(feature = "async-client")]
#[test]
fn cancelled_and_concurrent_shutdown_waiters_await_delivery_body_without_blocking_poll() {
    use std::future::Future;
    use std::sync::{mpsc, Mutex};
    use std::task::{Context, Poll};
    let server = MockServer::start();
    let rejected = server.mock(|when, then| {
        when.method(POST);
        then.status(400);
    });
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let mut setup = config(&server);
    setup.flush_at = 1;
    setup.on_error(move |failure| {
        if !matches!(failure, posthog::PostHogError::Capture(_)) {
            return;
        }
        entered_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(15))
            .unwrap();
    });
    let client = Arc::new(Client::init_blocking(setup));
    client.capture("rejected");
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let caller_client = client.clone();
    let (polled_tx, polled_rx) = mpsc::channel();
    let caller = std::thread::spawn(move || {
        let mut context = Context::from_waker(futures::task::noop_waker_ref());
        let mut cancelled = Box::pin(caller_client.shutdown(None));
        let first_pending = matches!(cancelled.as_mut().poll(&mut context), Poll::Pending);
        drop(cancelled);
        let mut second = Box::pin(caller_client.shutdown(Some(Duration::ZERO)));
        let second_pending = matches!(second.as_mut().poll(&mut context), Poll::Pending);
        polled_tx.send((first_pending, second_pending)).unwrap();
        futures::executor::block_on(second)
    });
    let polls = polled_rx.recv_timeout(Duration::from_secs(5));
    release_tx.send(()).unwrap();
    let report = caller.join().unwrap();
    assert_eq!(polls.unwrap(), (true, true));
    assert_eq!(report.delivery, DeliveryOutcome::Stopped);
    assert_eq!(client.capture("closed"), CaptureOutcome::Closed);
    rejected.assert_calls(1);
}

#[test]
fn drop_writes_pending_state_without_waiting_for_delivery() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST);
        then.delay(Duration::from_secs(30)).status(200);
    });
    let directory = std::env::temp_dir().join(format!("posthog-drop-{}", uuid::Uuid::now_v7()));
    let mut config = config(&server);
    config.persistence = Persistence::Directory(directory.clone());
    config.flush_at = 1;
    let client = Client::init_blocking(config);
    let id = client.get_distinct_id();
    assert!(matches!(
        client.capture("in-flight"),
        CaptureOutcome::Accepted { .. }
    ));
    let started = std::time::Instant::now();
    drop(client);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "drop waited for delivery"
    );
    let scope = std::fs::read_dir(&directory)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(scope.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["state"]["distinct_id"], id.as_str());
    let _ = std::fs::remove_dir_all(directory);
}

/// A persistent client that never sends: the host refuses connections and no
/// flush is due, so only `Drop` writes captured events.
fn unsent(directory: &Path) -> Config {
    let mut config = Config::new("phc_test");
    config.host = "http://127.0.0.1:9".into();
    config.persistence = Persistence::Directory(directory.to_path_buf());
    config.flush_interval = Duration::from_secs(3600);
    config.flush_at = 1_000;
    config
}

fn temporary_directory() -> PathBuf {
    std::env::temp_dir().join(format!("posthog-drop-{}", uuid::Uuid::now_v7()))
}

fn stored_events(directory: &Path) -> usize {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|scope| {
            std::fs::read_dir(scope.unwrap().path().join("events"))
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                        .count()
                })
                .unwrap_or(0)
        })
        .sum()
}

#[test]
fn drop_writes_captured_events_to_disk_before_returning() {
    let directory = temporary_directory();
    let client = Client::init_blocking(unsent(&directory));
    // Save the state first, so Drop has no state write to wait for.
    client.flush_blocking();
    for i in 0..50 {
        assert!(matches!(
            client.capture(format!("event-{}", i).as_str()),
            CaptureOutcome::Accepted { .. }
        ));
    }
    drop(client);
    let stored = stored_events(&directory);
    let _ = std::fs::remove_dir_all(&directory);
    assert_eq!(stored, 50);
}

#[test]
fn a_client_created_right_after_drop_restores_the_identity() {
    for _ in 0..10 {
        let directory = temporary_directory();
        let client = Client::init_blocking(unsent(&directory));
        let id = client.get_distinct_id();
        client.flush_blocking();
        for i in 0..20 {
            client.capture(format!("event-{}", i).as_str());
        }
        drop(client);
        let again = Client::init_blocking(unsent(&directory));
        let restored = again.get_distinct_id();
        drop(again);
        let _ = std::fs::remove_dir_all(&directory);
        assert_eq!(restored, id);
    }
}

#[cfg(feature = "async-client")]
#[test]
fn drop_after_a_cancelled_shutdown_does_not_wait_for_its_deadline() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST);
        then.delay(Duration::from_secs(30)).status(200);
    });
    let mut config = config(&server);
    config.flush_at = 1;
    config.shutdown_timeout = Duration::from_secs(8);
    let client = Client::init_blocking(config);
    assert!(matches!(
        client.capture("in-flight"),
        CaptureOutcome::Accepted { .. }
    ));
    std::thread::sleep(Duration::from_millis(200));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let waited = tokio::time::timeout(Duration::from_millis(200), client.shutdown(None)).await;
        assert!(waited.is_err(), "shutdown should still be waiting");
    });
    let started = std::time::Instant::now();
    drop(client);
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "drop waited {:?}",
        elapsed
    );
}

/// A persistent client whose uploads take 6 s, so one stays in flight while
/// an earlier shutdown selects an 8 s deadline.
fn slow_persistent(server: &MockServer, directory: &Path) -> Config {
    let mut config = config(server);
    config.persistence = Persistence::Directory(directory.to_path_buf());
    config
}

fn slow_server() -> MockServer {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST);
        then.delay(Duration::from_secs(6)).status(200);
    });
    server
}

/// Drop `client`, then create a client for the same namespace at once and
/// return its distinct ID.
fn distinct_id_after_drop<C>(client: C, server: &MockServer, directory: &Path) -> String {
    drop(client);
    let again = Client::init_blocking(slow_persistent(server, directory));
    let restored = again.get_distinct_id();
    drop(again);
    restored
}

#[cfg(feature = "async-client")]
#[test]
fn a_client_created_right_after_drop_following_a_cancelled_shutdown_restores_the_identity() {
    let server = slow_server();
    let directory = temporary_directory();
    let mut config = slow_persistent(&server, &directory);
    config.flush_at = 1;
    config.shutdown_timeout = Duration::from_secs(8);
    let client = Client::init_blocking(config);
    let id = client.get_distinct_id();
    client.flush_blocking();
    assert!(matches!(
        client.capture("in-flight"),
        CaptureOutcome::Accepted { .. }
    ));
    std::thread::sleep(Duration::from_millis(300));
    futures::executor::block_on(async {
        let shutdown = client.shutdown(None);
        futures::pin_mut!(shutdown);
        // Select the 8 s deadline, then cancel the wait.
        assert!(futures::poll!(shutdown.as_mut()).is_pending());
    });
    let restored = distinct_id_after_drop(client, &server, &directory);
    let _ = std::fs::remove_dir_all(&directory);
    assert_eq!(restored, id, "re-init right after Drop lost the identity");
}

#[test]
fn a_client_created_right_after_drop_following_a_hook_shutdown_restores_the_identity() {
    use std::sync::{Arc, Mutex, Weak};
    let server = slow_server();
    let directory = temporary_directory();
    let slot = Arc::new(Mutex::new(Weak::<Client>::new()));
    let hook_slot = slot.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = Mutex::new(tx);
    let mut config = slow_persistent(&server, &directory);
    config.flush_at = 1;
    config.shutdown_timeout = Duration::from_secs(8);
    config.before_send(move |event| {
        if event.event_name() == "stop" {
            if let Some(client) = hook_slot.lock().unwrap().upgrade() {
                let _ = tx.lock().unwrap().send(client.shutdown_blocking(None));
            }
        }
        Some(event)
    });
    let client = Arc::new(Client::init_blocking(config));
    *slot.lock().unwrap() = Arc::downgrade(&client);
    let id = client.get_distinct_id();
    client.flush_blocking();
    assert!(matches!(
        client.capture("in-flight"),
        CaptureOutcome::Accepted { .. }
    ));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(client.capture("stop"), CaptureOutcome::Closed);
    let report = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(report.delivery, DeliveryOutcome::Deferred);
    let restored = distinct_id_after_drop(client, &server, &directory);
    let _ = std::fs::remove_dir_all(&directory);
    assert_eq!(restored, id, "re-init right after Drop lost the identity");
}

/// A server that holds the default flags preload longer than the configured
/// 1 s request timeout, so the request would time out and be retried.
fn slow_flags(server: &MockServer) -> (httpmock::Mock<'_>, Config) {
    let flags = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200)
            .delay(Duration::from_secs(5))
            .json_body(serde_json::json!({"flags": {}}));
    });
    let mut setup = config(server);
    setup.preload_feature_flags = true;
    setup.request_timeout = Duration::from_secs(1);
    setup.shutdown_timeout = Duration::from_secs(10);
    (flags, setup)
}

fn wait_for_preload(flags: &httpmock::Mock<'_>) {
    let started = std::time::Instant::now();
    while flags.calls() == 0 && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(flags.calls(), 1, "the preload request did not start");
}

// Shutdown discards a flags result, so it neither waits for the request nor
// lets it retry, as Drop, posthog-ios and posthog-android do.
#[test]
fn shutdown_does_not_wait_for_or_retry_an_in_flight_flags_request() {
    let server = MockServer::start();
    let (flags, setup) = slow_flags(&server);
    let client = Client::init_blocking(setup);
    wait_for_preload(&flags);
    let started = std::time::Instant::now();
    let report = client.shutdown_blocking(None);
    let elapsed = started.elapsed();
    // Past the 1 s timeout plus the 300 ms backoff, when a retry would be sent.
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(report.delivery, DeliveryOutcome::Stopped);
    assert!(
        elapsed < Duration::from_secs(1),
        "shutdown waited {:?}",
        elapsed
    );
    assert_eq!(
        flags.calls(),
        1,
        "the flags request was retried after shutdown"
    );
}

// Shutdown discards the result of an in-flight flags request, so a failure
// that arrives afterwards is not reported either.
#[test]
fn a_flags_failure_arriving_after_shutdown_is_not_reported() {
    let server = MockServer::start();
    let flags = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        // 503 is not retried, so the failure is reported as soon as it arrives.
        then.status(503).delay(Duration::from_millis(500));
    });
    let mut setup = config(&server);
    setup.preload_feature_flags = true;
    let (errors_tx, errors) = std::sync::mpsc::channel();
    setup.on_error(move |_| {
        let _ = errors_tx.send(());
    });
    let client = Client::init_blocking(setup);
    wait_for_preload(&flags);
    client.shutdown_blocking(None);
    drop(client);
    // Well past the delayed response.
    assert!(
        errors.recv_timeout(Duration::from_secs(2)).is_err(),
        "on_error ran after shutdown returned"
    );
}

#[cfg(feature = "async-client")]
#[test]
fn async_shutdown_does_not_wait_for_an_in_flight_flags_request() {
    let server = MockServer::start();
    let (flags, setup) = slow_flags(&server);
    let client = Client::init_blocking(setup);
    wait_for_preload(&flags);
    let started = std::time::Instant::now();
    let report = futures::executor::block_on(client.shutdown(None));
    let elapsed = started.elapsed();
    assert_eq!(report.delivery, DeliveryOutcome::Stopped);
    assert!(
        elapsed < Duration::from_secs(1),
        "shutdown waited {:?}",
        elapsed
    );
}

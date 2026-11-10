#![cfg(feature = "stateful")]

#[cfg(feature = "async-client")]
use std::sync::Arc;
use std::time::Duration;

use httpmock::{Method::POST, MockServer};
use posthog::stateful::{
    CaptureOutcome, Client, Config, DeliveryOutcome, Persistence, PersistenceOutcome,
};

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
        then.status(200);
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
    let batch = server.mock(|when, then| {
        when.method(POST);
        then.status(200);
    });
    let mut setup = config(&server);
    setup.persistence = Persistence::Directory(directory.0.clone());
    let client = Client::init_blocking(setup);
    client.flush_blocking(); // Wait for initial recovery reservations.
    let identity = client.get_distinct_id();
    assert!(matches!(
        client.capture("persisted"),
        CaptureOutcome::Accepted { .. }
    ));
    assert_eq!(
        client.shutdown_blocking(Some(Duration::ZERO)).delivery,
        DeliveryOutcome::Stopped
    );
    batch.assert_calls(0);
    let scope = std::fs::read_dir(&directory.0)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
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
            then.status(200);
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
    setup.on_error(move |_| {
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
    assert_eq!(
        futures::executor::block_on(client.shutdown(None)).delivery,
        DeliveryOutcome::Stopped
    );
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
    setup.on_error(move |_| {
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

use std::io;
use std::net::TcpListener;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use httpmock::{Method::POST, MockServer};

use super::storage_tests::{wire, TestDirectory};
use super::tests::options;
use super::{
    Admission, Clock, LaneConfig, StartupFailure, StorageState, SystemClock, TransportHandle,
};
use crate::capture_event::CaptureEvent;
use crate::client::event_store::{record_size, EventStore, FileEventStore, Recovery, StoreKey};

const WAIT: Duration = Duration::from_secs(5);

#[test]
fn ready_memory_transport_does_not_connect_and_has_a_byte_bound() {
    // A listening socket that never responds is not a working ingestion server.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let event = wire("bounded-memory");
    let bytes = record_size(&event);
    let handle = TransportHandle::spawn_stateful(
        options(format!("http://{}", listener.local_addr().unwrap()))
            .max_queue_size(2usize)
            .flush_interval_ms(60_000u64)
            .request_timeout_seconds(1u64)
            .shutdown_timeout_ms(0u64)
            .build()
            .unwrap(),
        None,
        bytes,
    )
    .expect("local initialization does not require a reachable ingestion service");
    assert_eq!(handle.enqueue_prepared(event.clone()), Admission::Accepted);
    assert_eq!(handle.enqueue_prepared(event), Admission::Full);
    assert_eq!(handle.pending(), 1);
    handle.shutdown_blocking();
    assert_eq!(handle.byte_budget.as_ref().unwrap().available(), bytes);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

struct StartupClock {
    gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}

impl Clock for StartupClock {
    fn now(&self) -> Instant {
        let gate = self.gate.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            entered.send(()).unwrap();
            release.recv_timeout(WAIT).unwrap();
        }
        Instant::now()
    }

    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[test]
fn readiness_waits_for_worker_initialization() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let clock = StartupClock {
        gate: Mutex::new(Some((entered_tx, release_rx))),
    };
    let (ready_tx, ready_rx) = mpsc::channel();
    let handle = TransportHandle::spawn_configured_with_startup(
        options("http://127.0.0.1:1".into()).build().unwrap(),
        LaneConfig::analytics(),
        Arc::new(clock),
        StorageState::memory(4096),
        Some(ready_tx),
    );
    let (finished_tx, finished_rx) = mpsc::channel();
    let initializer = std::thread::spawn(move || {
        finished_tx.send(handle.finish_startup(ready_rx)).ok();
    });
    entered_rx.recv_timeout(WAIT).unwrap();
    let before_release = finished_rx.try_recv();
    release_tx.send(()).unwrap();
    assert!(matches!(before_release, Err(mpsc::TryRecvError::Empty)));
    let handle = finished_rx
        .recv_timeout(WAIT)
        .unwrap()
        .expect("worker initialized");
    initializer.join().unwrap();
    handle.shutdown_blocking();
}

struct GatedRecovery {
    inner: FileEventStore,
    entered: Option<mpsc::Sender<()>>,
    release: mpsc::Receiver<()>,
}

impl EventStore for GatedRecovery {
    fn recover(&mut self, events: usize, bytes: u64) -> io::Result<Recovery> {
        if let Some(entered) = self.entered.take() {
            entered.send(()).unwrap();
            self.release.recv_timeout(WAIT).unwrap();
        }
        self.inner.recover(events, bytes)
    }

    fn put(&mut self, event: &CaptureEvent) -> io::Result<StoreKey> {
        self.inner.put(event)
    }

    fn remove(&mut self, key: StoreKey) -> io::Result<()> {
        self.inner.remove(key)
    }
}

#[test]
fn readiness_does_not_wait_for_recovery_and_preserves_shared_admission() {
    let server = MockServer::start();
    let directory = TestDirectory::new();
    let recovered = wire("recovered");
    {
        let mut store = directory.open();
        store.put(&recovered).unwrap();
    }
    let store = directory.open();
    let fresh = wire("fresh");
    let delivery = server.mock(|when, then| {
        when.method(POST);
        then.status(200).json_body(serde_json::json!({"results": {
            (recovered.uuid.to_string()): {"result": "ok"},
            (fresh.uuid.to_string()): {"result": "ok"}
        }}));
    });
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    let config = options(server.base_url())
        .max_queue_size(2usize)
        .before_send(|_| panic!("prepared and recovered events must not rerun hooks"))
        .build()
        .unwrap();
    let initializer = std::thread::spawn(move || {
        finished_tx
            .send(TransportHandle::spawn_stateful(
                config,
                Some(Box::new(GatedRecovery {
                    inner: store,
                    entered: Some(entered_tx),
                    release: release_rx,
                })),
                64 * 1024,
            ))
            .ok();
    });
    let result = finished_rx.recv_timeout(WAIT);
    // Release the backend on a failed readiness assertion as well.
    if result.is_err() {
        let _ = release_tx.send(());
    }
    let handle = result.unwrap().expect("local transport is ready");
    entered_rx.recv_timeout(WAIT).unwrap();
    // Recovery owns the window while its read is blocked. Ready does not mean
    // the queue has free capacity, and capture does not wait for that read.
    let during_recovery = handle.enqueue_prepared(fresh.clone());
    release_tx.send(()).unwrap();
    assert_eq!(during_recovery, Admission::Full);
    handle.tick();
    assert_eq!(handle.enqueue_prepared(fresh), Admission::Accepted);
    handle.flush_blocking();
    delivery.assert_calls(2);
    assert!(directory.records().is_empty());
    handle.shutdown_blocking();
    initializer.join().unwrap();
    drop(directory.open()); // Shutdown released the namespace lease.
}

#[test]
fn failed_startup_acknowledgment_joins_worker_and_releases_namespace() {
    let directory = TestDirectory::new();
    let handle = TransportHandle::spawn_configured(
        options("http://127.0.0.1:1".into()).build().unwrap(),
        LaneConfig::analytics(),
        Arc::new(SystemClock),
        StorageState::new(Box::new(directory.open()), 4096),
    );
    let (tx, rx) = mpsc::channel();
    // Exercise the receiver's failed-HTTP outcome without relying on a broken
    // system TLS installation. The shared worker itself is healthy here.
    tx.send(false).unwrap();
    assert_eq!(handle.finish_startup(rx).err(), Some(StartupFailure::Http));
    drop(directory.open());
}

struct PanickingClock;

impl Clock for PanickingClock {
    fn now(&self) -> Instant {
        panic!("injected initialization failure");
    }

    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[test]
fn worker_exit_before_acknowledgment_wakes_initializer_and_releases_namespace() {
    let directory = TestDirectory::new();
    let (tx, rx) = mpsc::channel();
    let handle = TransportHandle::spawn_configured_with_startup(
        options("http://127.0.0.1:1".into()).build().unwrap(),
        LaneConfig::analytics(),
        Arc::new(PanickingClock),
        StorageState::new(Box::new(directory.open()), 4096),
        Some(tx),
    );
    let (finished_tx, finished_rx) = mpsc::channel();
    let initializer = std::thread::spawn(move || {
        finished_tx.send(handle.finish_startup(rx).err()).unwrap();
    });
    assert_eq!(
        finished_rx.recv_timeout(WAIT).unwrap(),
        Some(StartupFailure::WorkerStopped)
    );
    initializer.join().unwrap();
    drop(directory.open());
}

//! Starting stateful delivery: readiness, failures, and the storage lease.

use std::fs;
use std::io;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use httpmock::{Method::POST, MockServer};
use uuid::Uuid;

use super::delivery::{Admission, StartupFailure};
use super::tests::options;
use super::{SystemClock, TransportHandle};
use crate::capture_event::CaptureEvent;
use crate::client::event_store::{
    record_size, EventStore, FileEventStore, StorageDirectory, StoreKey, StoreLimits,
};
use crate::client::ClientOptions;
use crate::Event;

const WAIT: Duration = Duration::from_secs(5);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("posthog-startup-{}", Uuid::now_v7())))
    }

    fn open(&self) -> FileEventStore {
        FileEventStore::open(
            StorageDirectory::open(&self.0).unwrap(),
            StoreLimits {
                max_events: 20,
                max_bytes: 128 * 1024,
            },
        )
        .unwrap()
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn wire(name: &str) -> CaptureEvent {
    CaptureEvent::from_event(&Event::new(name, "user"))
}

fn spawn_with(
    store: FileEventStore,
    build_http: fn(&ClientOptions) -> Option<reqwest::blocking::Client>,
) -> Result<TransportHandle, StartupFailure> {
    TransportHandle::spawn_stateful_with(
        options("http://127.0.0.1:1".into()).build().unwrap(),
        Some(Box::new(store)),
        4096,
        Arc::new(Mutex::new(None)),
        Arc::new(SystemClock),
        build_http,
    )
}

#[test]
fn a_memory_transport_starts_without_connecting_and_bounds_what_it_holds() {
    // A listening socket that never responds is not a working ingestion server.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let event = wire("bounded-memory");
    let bytes = record_size(&event);
    let handle = TransportHandle::spawn_stateful(
        options(format!("http://{}", listener.local_addr().unwrap()))
            .flush_interval_ms(60_000u64)
            .shutdown_timeout_ms(0u64)
            .build()
            .unwrap(),
        None,
        bytes,
    )
    .expect("startup does not need a reachable ingestion service");
    assert_eq!(handle.enqueue_prepared(event.clone()), Admission::Accepted);
    assert_eq!(handle.enqueue_prepared(event), Admission::Accepted);
    // Room for one event: the newest is kept.
    let deadline = std::time::Instant::now() + WAIT;
    while handle.pending() != 1 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    handle.shutdown_blocking();
    assert_eq!(handle.pending(), 0);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

/// A store whose first read waits until the test releases it.
struct GatedRead {
    inner: FileEventStore,
    entered: Option<mpsc::Sender<()>>,
    release: mpsc::Receiver<()>,
}

impl EventStore for GatedRead {
    fn limits(&self) -> StoreLimits {
        self.inner.limits()
    }
    fn take_index(&mut self) -> Vec<(StoreKey, u64)> {
        self.inner.take_index()
    }
    fn last_key(&self) -> StoreKey {
        self.inner.last_key()
    }
    fn write(&mut self, key: StoreKey, event: &CaptureEvent) -> io::Result<u64> {
        self.inner.write(key, event)
    }
    fn read(&mut self, key: StoreKey) -> io::Result<CaptureEvent> {
        if let Some(entered) = self.entered.take() {
            entered.send(()).unwrap();
            self.release.recv_timeout(WAIT).unwrap();
        }
        self.inner.read(key)
    }
    fn delete(&mut self, key: StoreKey) -> io::Result<()> {
        self.inner.delete(key)
    }
}

#[test]
fn startup_does_not_wait_for_the_backlog_and_admission_works_meanwhile() {
    let server = MockServer::start();
    let delivery = server.mock(|when, then| {
        when.method(POST);
        then.status(200)
            .json_body(serde_json::json!({"results": {}}));
    });
    let directory = TestDirectory::new();
    directory
        .open()
        .write(StoreKey(1), &wire("earlier"))
        .unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let handle = TransportHandle::spawn_stateful(
        options(server.base_url())
            .before_send(|_| panic!("queued events never rerun hooks"))
            .build()
            .unwrap(),
        Some(Box::new(GatedRead {
            inner: directory.open(),
            entered: Some(entered_tx),
            release: release_rx,
        })),
        64 * 1024,
    )
    .expect("startup returns while the backlog is being read");
    entered_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(handle.enqueue_prepared(wire("fresh")), Admission::Accepted);
    release_tx.send(()).unwrap();
    handle.flush_blocking();
    assert!(delivery.calls() >= 1);
    handle.shutdown_blocking();
    // Shutdown released the namespace lease.
    drop(directory.open());
}

#[test]
fn a_failed_http_setup_reports_it_and_releases_the_namespace() {
    let directory = TestDirectory::new();
    let result = spawn_with(directory.open(), |_| None);
    assert_eq!(result.err(), Some(StartupFailure::Http));
    drop(directory.open());
}

#[test]
fn an_executor_that_stops_before_ready_reports_it_and_releases_the_namespace() {
    let directory = TestDirectory::new();
    let result = spawn_with(directory.open(), |_| panic!("injected startup failure"));
    assert_eq!(result.err(), Some(StartupFailure::WorkerStopped));
    drop(directory.open());
}

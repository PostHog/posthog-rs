use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use httpmock::{Method::POST, MockServer};
use uuid::Uuid;

use super::tests::{options, ManualClock};
use super::{Admission, CaptureEvent, LaneConfig, StorageState, TransportHandle};
use crate::client::event_store::{
    EventStore, FileEventStore, Recovery, StorageDirectory, StoreKey, StoreLimits,
};
use crate::{Event, PostHogError, StorageOperation};

impl TransportHandle {
    /// Storage integration fixture with the normal asynchronous worker startup.
    fn spawn_persistent(
        options: crate::ClientOptions,
        store: Box<dyn EventStore>,
        max_queue_bytes: usize,
    ) -> Self {
        Self::spawn_configured(
            options,
            LaneConfig::analytics(),
            Arc::new(super::SystemClock),
            StorageState::new(store, max_queue_bytes),
        )
    }
}

pub(super) struct TestDirectory(PathBuf);

impl TestDirectory {
    pub(super) fn new() -> Self {
        Self(std::env::temp_dir().join(format!("posthog-delivery-{}", Uuid::now_v7())))
    }

    pub(super) fn open(&self) -> FileEventStore {
        FileEventStore::open(
            StorageDirectory::open(&self.0).unwrap(),
            StoreLimits {
                max_events: 20,
                max_bytes: 128 * 1024,
                max_record_bytes: 4096,
            },
        )
        .unwrap()
    }

    pub(super) fn records(&self) -> Vec<PathBuf> {
        let mut paths: Vec<_> = fs::read_dir(self.0.join("events"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension().and_then(|extension| extension.to_str()) == Some("json")
            })
            .collect();
        paths.sort();
        paths
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn wire(name: &str) -> CaptureEvent {
    let mut wire = CaptureEvent::from_event(&Event::new(name, "original-user"));
    wire.timestamp = "2025-01-01T00:00:00.123Z".into();
    wire.session_id = Some("original-session".into());
    wire.properties = serde_json::json!({"snapshot": name});
    wire
}

fn persistent(
    server: &MockServer,
    clock: &ManualClock,
    store: impl EventStore + 'static,
) -> TransportHandle {
    TransportHandle::spawn_configured(
        options(server.base_url())
            .max_capture_attempts(1u32)
            .build()
            .unwrap(),
        LaneConfig::analytics(),
        Arc::new(clock.clone()),
        StorageState::new(Box::new(store), 64 * 1024),
    )
}

#[test]
fn mixed_capture_time_sdk_versions_survive_recovery_and_partial_retry() {
    let server = MockServer::start();
    let directory = TestDirectory::new();
    let now = "2025-01-01T00:00:00.123456789Z".parse().unwrap();
    let snapshots: Vec<_> = [("old-sdk", "1.0.0"), ("new-sdk", "2.0.0")]
        .iter()
        .map(|&(lib, version)| {
            let mut input = Event::new("captured", "anonymous-id");
            input.set_uuid(Uuid::now_v7());
            input.insert_prop("$lib", lib).unwrap();
            input.insert_prop("$lib_version", version).unwrap();
            CaptureEvent::from_stateful_event_at(&input, now)
        })
        .collect();
    let old = &snapshots[0];
    let new = &snapshots[1];
    let old_json = serde_json::to_string(old).unwrap();
    let new_json = serde_json::to_string(new).unwrap();
    {
        let mut store = directory.open();
        for snapshot in &snapshots {
            store.put(snapshot).unwrap();
        }
    }
    let mut partial = server.mock(|when, then| {
        when.method(POST)
            .header("posthog-sdk-info", crate::client::get_default_user_agent())
            .body_includes(&old_json)
            .body_includes(&new_json);
        then.status(200).json_body(serde_json::json!({"results": {
            (old.uuid.to_string()): {"result": "retry"},
            (new.uuid.to_string()): {"result": "ok"}
        }}));
    });
    let hook_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let worker_options = || {
        let calls = hook_calls.clone();
        options(server.base_url())
            .max_capture_attempts(1u32)
            .shutdown_timeout_ms(0u64)
            .before_send(move |_| {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                None
            })
            .build()
            .unwrap()
    };
    let handle = TransportHandle::spawn_configured(
        worker_options(),
        LaneConfig::analytics(),
        Arc::new(ManualClock::new()),
        StorageState::new(Box::new(directory.open()), 64 * 1024),
    );
    handle.flush_blocking();
    partial.assert_calls(1);
    assert_eq!(directory.records().len(), 1);
    let retained: serde_json::Value =
        serde_json::from_slice(&fs::read(&directory.records()[0]).unwrap()).unwrap();
    assert_eq!(retained["event"], serde_json::to_value(old).unwrap());
    handle.shutdown_blocking();
    partial.delete();

    let recovered = server.mock(|when, then| {
        when.method(POST)
            .header("posthog-sdk-info", crate::client::get_default_user_agent())
            .body_includes(&old_json)
            .body_excludes(&new_json);
        then.status(200).json_body(serde_json::json!({"results": {
            (old.uuid.to_string()): {"result": "ok"}
        }}));
    });
    let handle = TransportHandle::spawn_stateful(
        worker_options(),
        Some(Box::new(directory.open())),
        64 * 1024,
    )
    .unwrap();
    handle.flush_blocking();
    handle.shutdown_blocking();
    recovered.assert_calls(1);
    assert!(directory.records().is_empty());
    assert_eq!(hook_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn transient_exhaustion_and_shutdown_preserve_records_for_restart() {
    let server = MockServer::start();
    let directory = TestDirectory::new();
    let clock = ManualClock::new();
    let event = wire("retained");
    let json = serde_json::to_string(&event).unwrap();
    let mut offline = server.mock(|when, then| {
        when.method(POST).body_includes(&json);
        then.status(503);
    });
    let handle = persistent(&server, &clock, directory.open());
    assert_eq!(handle.enqueue_prepared(event.clone()), Admission::Accepted);
    handle.flush_blocking();
    offline.assert_calls(1);
    assert_eq!(handle.pending(), 1);
    assert_eq!(directory.records().len(), 1);
    // Explicit flush does not bypass a retained sequence's cooldown.
    handle.flush_blocking();
    offline.assert_calls(1);
    handle.shutdown_blocking();
    assert_eq!(handle.pending(), 0);
    assert_eq!(directory.records().len(), 1);

    offline.delete();
    let restored = server.mock(|when, then| {
        when.method(POST).body_includes(&json);
        then.status(200).json_body(serde_json::json!({"results": {
            (event.uuid.to_string()): {"result": "ok"}
        }}));
    });
    let handle = TransportHandle::spawn_configured(
        options(server.base_url())
            .before_send(|_| panic!("restoration must not rerun hooks"))
            .build()
            .unwrap(),
        LaneConfig::analytics(),
        Arc::new(clock),
        StorageState::new(Box::new(directory.open()), 64 * 1024),
    );
    handle.flush_blocking();
    restored.assert_calls(1);
    assert_eq!(handle.pending(), 0);
    assert!(directory.records().is_empty());
    handle.shutdown_blocking();
}

#[test]
fn partial_acknowledgement_removes_exact_records_including_duplicate_uuids() {
    let server = MockServer::start();
    let directory = TestDirectory::new();
    let clock = ManualClock::new();
    let first = wire("first-copy");
    let mut duplicate = first.clone();
    duplicate.event = "duplicate-copy".into();
    let accepted = wire("accepted");
    let mut partial = server.mock(|when, then| {
        when.method(POST);
        then.status(200).json_body(serde_json::json!({"results": {
            (first.uuid.to_string()): {"result": "retry", "duplicates_dropped": 1},
            (accepted.uuid.to_string()): {"result": "ok"}
        }}));
    });
    let handle = persistent(&server, &clock, directory.open());
    assert_eq!(handle.enqueue_prepared(first.clone()), Admission::Accepted);
    assert_eq!(handle.enqueue_prepared(duplicate), Admission::Accepted);
    assert_eq!(handle.enqueue_prepared(accepted), Admission::Accepted);
    handle.flush_blocking();
    partial.assert_calls(1);
    assert_eq!(handle.pending(), 1);
    assert_eq!(
        handle.byte_budget.as_ref().unwrap().available(),
        64 * 1024 - crate::client::event_store::record_size(&first)
    );
    let records = directory.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].file_stem().unwrap(), "00000000000000000001");
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(&records[0]).unwrap()).unwrap();
    assert_eq!(persisted["event"]["event"], "first-copy");

    partial.delete();
    let success = server.mock(|when, then| {
        when.method(POST)
            .body_excludes("duplicate-copy")
            .body_excludes("accepted");
        then.status(200).json_body(serde_json::json!({"results": {
            (first.uuid.to_string()): {"result": "ok"}
        }}));
    });
    clock.advance(Duration::from_secs(11));
    handle.tick();
    success.assert_calls(1);
    assert_eq!(handle.pending(), 0);
    assert_eq!(handle.byte_budget.as_ref().unwrap().available(), 64 * 1024);
    assert!(directory.records().is_empty());
    handle.shutdown_blocking();
}

struct BlockedWriteFailure {
    entered: Option<mpsc::Sender<std::thread::ThreadId>>,
    release: mpsc::Receiver<()>,
}

impl EventStore for BlockedWriteFailure {
    fn recover(&mut self, _: usize, _: u64) -> io::Result<Recovery> {
        Ok(Recovery::default())
    }

    fn put(&mut self, _: &CaptureEvent) -> io::Result<StoreKey> {
        if let Some(entered) = self.entered.take() {
            entered.send(std::thread::current().id()).unwrap();
            self.release.recv().unwrap();
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "test write failure",
        ))
    }

    fn remove(&mut self, _: StoreKey) -> io::Result<()> {
        panic!("an event whose write failed has no acknowledged storage key")
    }
}

#[test]
fn blocked_disk_does_not_block_capture_and_failed_writes_report_on_error() {
    let server = MockServer::start();
    let event = wire("memory-fallback");
    let success = server.mock(|when, then| {
        when.method(POST)
            .body_includes(serde_json::to_string(&event).unwrap());
        then.status(200).json_body(serde_json::json!({"results": {
            (event.uuid.to_string()): {"result": "ok"}
        }}));
    });
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (error_tx, error_rx) = mpsc::channel();
    let handle = Arc::new(TransportHandle::spawn_persistent(
        options(server.base_url())
            .on_error(move |failure| {
                if let PostHogError::Storage(failure) = failure {
                    error_tx
                        .send((
                            failure.operation(),
                            failure.error().kind(),
                            std::thread::current().id(),
                        ))
                        .unwrap();
                }
            })
            .build()
            .unwrap(),
        Box::new(BlockedWriteFailure {
            entered: Some(entered_tx),
            release: release_rx,
        }),
        64 * 1024,
    ));
    let producer_handle = Arc::clone(&handle);
    let (admitted_tx, admitted_rx) = mpsc::channel();
    let producer = std::thread::spawn(move || {
        admitted_tx
            .send(producer_handle.enqueue_prepared(event))
            .unwrap();
    });
    let worker_id = entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let admitted = admitted_rx.recv_timeout(Duration::from_secs(1));
    release_tx.send(()).unwrap();
    let producer_id = producer.thread().id();
    producer.join().unwrap();
    assert_eq!(admitted.unwrap(), Admission::Accepted);
    assert_ne!(worker_id, producer_id);
    handle.flush_blocking();
    success.assert_calls(1);
    let (operation, kind, callback_thread) = error_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(operation, StorageOperation::Write);
    assert_eq!(kind, io::ErrorKind::PermissionDenied);
    assert_eq!(callback_thread, worker_id);
    assert_eq!(handle.pending(), 0);
    handle.shutdown_blocking();
}

#[test]
fn unsupported_file_record_reports_write_error_and_delivers_from_memory() {
    let server = MockServer::start();
    let directory = TestDirectory::new();
    let mut event = wire("deep-memory-fallback");
    let mut properties = serde_json::json!("private leaf");
    for _ in 0..130 {
        properties = serde_json::json!({"nested": properties});
    }
    event.properties = properties;
    let success = server.mock(|when, then| {
        when.method(POST)
            .body_includes(serde_json::to_string(&event).unwrap());
        then.status(200).json_body(serde_json::json!({"results": {
            (event.uuid.to_string()): {"result": "ok"}
        }}));
    });
    let bytes = crate::client::event_store::record_size(&event);
    let (error_tx, error_rx) = mpsc::channel();
    let handle = TransportHandle::spawn_persistent(
        options(server.base_url())
            .on_error(move |failure| {
                if let PostHogError::Storage(failure) = failure {
                    error_tx
                        .send((
                            failure.operation(),
                            failure.error().kind(),
                            failure.error().to_string(),
                            std::thread::current().id(),
                        ))
                        .unwrap();
                }
            })
            .build()
            .unwrap(),
        Box::new(directory.open()),
        bytes,
    );
    // Recovery temporarily reserves the byte budget, even for an empty store.
    // Finish it before admitting this record at the exact budget limit.
    handle.tick();
    assert_eq!(handle.enqueue_prepared(event), Admission::Accepted);
    handle.flush_blocking();
    let (operation, kind, message, callback_thread) =
        error_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(operation, StorageOperation::Write);
    assert_eq!(kind, io::ErrorKind::InvalidData);
    assert_eq!(message, "Invalid persisted event record");
    assert_ne!(callback_thread, std::thread::current().id());
    success.assert_calls(1);
    assert_eq!(handle.pending(), 0);
    assert_eq!(handle.byte_budget.as_ref().unwrap().available(), bytes);
    assert_eq!(fs::read_dir(directory.0.join("events")).unwrap().count(), 0);
    handle.shutdown_blocking();
    assert!(error_rx.try_recv().is_err());
    let recovered = directory.open().recover(20, 128 * 1024).unwrap();
    assert!(recovered.events.is_empty());
    assert!(recovered.errors.is_empty());
}

struct RemovalFailure(FileEventStore);

impl EventStore for RemovalFailure {
    fn recover(&mut self, events: usize, bytes: u64) -> io::Result<Recovery> {
        self.0.recover(events, bytes)
    }
    fn put(&mut self, event: &CaptureEvent) -> io::Result<StoreKey> {
        self.0.put(event)
    }
    fn remove(&mut self, _: StoreKey) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "test remove failure",
        ))
    }
}

#[test]
fn removal_failure_reports_degraded_durability_without_retrying_delivered_events() {
    let server = MockServer::start();
    let directory = TestDirectory::new();
    let event = wire("delivered");
    let success = server.mock(|when, then| {
        when.method(POST);
        then.status(200).json_body(serde_json::json!({"results": {
            (event.uuid.to_string()): {"result": "ok"}
        }}));
    });
    let (error_tx, error_rx) = mpsc::channel();
    let handle = TransportHandle::spawn_persistent(
        options(server.base_url())
            .on_error(move |failure| {
                if let PostHogError::Storage(failure) = failure {
                    error_tx.send(failure.operation()).unwrap();
                }
            })
            .build()
            .unwrap(),
        Box::new(RemovalFailure(directory.open())),
        64 * 1024,
    );
    assert_eq!(handle.enqueue_prepared(event), Admission::Accepted);
    handle.flush_blocking();
    assert_eq!(
        error_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        StorageOperation::Remove
    );
    assert_eq!(handle.pending(), 0);
    assert_eq!(directory.records().len(), 1);
    handle.flush_blocking();
    success.assert_calls(1);
    handle.shutdown_blocking();
    // Restart can redeliver the uncertain acknowledgement; original UUID stays.
    let handle = TransportHandle::spawn_persistent(
        options(server.base_url()).build().unwrap(),
        Box::new(directory.open()),
        64 * 1024,
    );
    handle.flush_blocking();
    success.assert_calls(2);
    assert!(directory.records().is_empty());
    handle.shutdown_blocking();
}

#[test]
fn prepared_memory_admission_is_byte_bounded_and_releases_terminal_reservations() {
    let server = MockServer::start();
    let event = wire("one-record");
    let bytes = crate::client::event_store::record_size(&event);
    let success = server.mock(|when, then| {
        when.method(POST);
        then.status(200).json_body(serde_json::json!({"results": {
            (event.uuid.to_string()): {"result": "ok"}
        }}));
    });
    let handle = TransportHandle::spawn_configured(
        options(server.base_url()).build().unwrap(),
        LaneConfig::analytics(),
        Arc::new(ManualClock::new()),
        StorageState::memory(bytes),
    );
    let budget = handle.byte_budget.as_ref().unwrap();
    assert_eq!(handle.enqueue_prepared(event.clone()), Admission::Accepted);
    assert_eq!(budget.available(), 0);
    assert_eq!(handle.enqueue_prepared(event.clone()), Admission::Full);
    assert_eq!(handle.pending(), 1);
    handle.flush_blocking();
    success.assert_calls(1);
    assert_eq!(handle.pending(), 0);
    assert_eq!(budget.available(), bytes);
    let mut oversized = event.clone();
    oversized.properties = serde_json::json!({"large": "x".repeat(bytes)});
    assert_eq!(handle.enqueue_prepared(oversized), Admission::Full);
    assert_eq!(handle.pending(), 0);
    assert_eq!(budget.available(), bytes);
    assert_eq!(handle.enqueue_prepared(event.clone()), Admission::Accepted);
    handle.shutdown_blocking();
    success.assert_calls(2);
    assert_eq!(budget.available(), bytes);
    assert_eq!(handle.enqueue_prepared(event), Admission::Closed);
    assert_eq!(budget.available(), bytes);
}

#[test]
fn write_failure_memory_fallback_remains_byte_bounded_while_offline() {
    let server = MockServer::start();
    let event = wire("offline");
    let bytes = crate::client::event_store::record_size(&event);
    let offline = server.mock(|when, then| {
        when.method(POST);
        then.status(503);
    });
    let (_, release) = mpsc::channel();
    let (error_tx, error_rx) = mpsc::channel();
    let handle = TransportHandle::spawn_configured(
        options(server.base_url())
            .max_capture_attempts(1u32)
            .on_error(move |failure| {
                if let PostHogError::Storage(failure) = failure {
                    error_tx.send(failure.operation()).unwrap();
                }
            })
            .build()
            .unwrap(),
        LaneConfig::analytics(),
        Arc::new(ManualClock::new()),
        StorageState::new(
            Box::new(BlockedWriteFailure {
                entered: None,
                release,
            }),
            bytes,
        ),
    );
    assert_eq!(handle.enqueue_prepared(event.clone()), Admission::Accepted);
    handle.flush_blocking();
    assert_eq!(
        error_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        StorageOperation::Write
    );
    offline.assert_calls(1);
    assert_eq!(handle.enqueue_prepared(event), Admission::Full);
    assert_eq!(handle.pending(), 1);
    assert_eq!(handle.byte_budget.as_ref().unwrap().available(), 0);
    handle.shutdown_blocking();
    assert_eq!(handle.pending(), 0);
    assert_eq!(handle.byte_budget.as_ref().unwrap().available(), bytes);
}

#[test]
fn recovery_and_retry_share_the_same_byte_budget_across_pages() {
    let server = MockServer::start();
    let directory = TestDirectory::new();
    let events: Vec<_> = (0..3)
        .map(|index| wire(&format!("event-{index}")))
        .collect();
    let bytes = crate::client::event_store::record_size(&events[0]);
    {
        let mut store = directory.open();
        for event in &events {
            store.put(event).unwrap();
        }
    }
    assert_eq!(
        fs::metadata(&directory.records()[0]).unwrap().len(),
        bytes as u64
    );
    let mut offline = server.mock(|when, then| {
        when.method(POST);
        then.status(503);
    });
    let clock = ManualClock::new();
    let handle = TransportHandle::spawn_configured(
        options(server.base_url())
            .max_queue_size(10usize)
            .max_capture_attempts(1u32)
            .build()
            .unwrap(),
        LaneConfig::analytics(),
        Arc::new(clock.clone()),
        StorageState::new(Box::new(directory.open()), 2 * bytes),
    );
    handle.flush_blocking();
    offline.assert_calls(1);
    assert_eq!(handle.pending(), 2);
    assert_eq!(handle.byte_budget.as_ref().unwrap().available(), 0);
    assert_eq!(directory.records().len(), 3);
    assert_eq!(handle.enqueue_prepared(wire("new-event")), Admission::Full);
    assert_eq!(handle.pending(), 2);
    offline.delete();
    let results: serde_json::Map<_, _> = events
        .iter()
        .map(|event| (event.uuid.to_string(), serde_json::json!({"result": "ok"})))
        .collect();
    let success = server.mock(|when, then| {
        when.method(POST);
        then.status(200)
            .json_body(serde_json::json!({"results": results}));
    });
    clock.advance(Duration::from_secs(11));
    handle.tick();
    success.assert_calls(2);
    assert_eq!(handle.pending(), 0);
    assert_eq!(handle.byte_budget.as_ref().unwrap().available(), 2 * bytes);
    assert!(directory.records().is_empty());
    handle.shutdown_blocking();
}

struct BlockedRecovery {
    inner: FileEventStore,
    entered: Option<mpsc::Sender<(usize, u64)>>,
    release: mpsc::Receiver<()>,
}

impl EventStore for BlockedRecovery {
    fn recover(&mut self, events: usize, bytes: u64) -> io::Result<Recovery> {
        if let Some(entered) = self.entered.take() {
            entered.send((events, bytes)).unwrap();
            self.release.recv().unwrap();
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
fn recovery_reserves_capacity_before_io_and_pages_a_larger_old_backlog() {
    let server = MockServer::start();
    let directory = TestDirectory::new();
    let events: Vec<_> = (0..5)
        .map(|index| wire(&format!("event-{index}")))
        .collect();
    {
        let mut store = directory.open();
        for event in &events {
            store.put(event).unwrap();
        }
    }
    let results: serde_json::Map<_, _> = events
        .iter()
        .map(|event| (event.uuid.to_string(), serde_json::json!({"result": "ok"})))
        .collect();
    let success = server.mock(|when, then| {
        when.method(POST);
        then.status(200)
            .json_body(serde_json::json!({"results": results}));
    });
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let handle = TransportHandle::spawn_configured(
        options(server.base_url())
            .max_queue_size(2usize)
            .max_batch_size(2usize)
            .build()
            .unwrap(),
        LaneConfig::analytics(),
        Arc::new(ManualClock::new()),
        StorageState::new(
            Box::new(BlockedRecovery {
                inner: directory.open(),
                entered: Some(entered_tx),
                release: release_rx,
            }),
            64 * 1024,
        ),
    );
    let requested = entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let admission = handle.enqueue_prepared(wire("new-event"));
    let available = handle.byte_budget.as_ref().unwrap().available();
    release_tx.send(()).unwrap();
    assert_eq!(requested, (2, 64 * 1024));
    assert_eq!(admission, Admission::Full);
    assert_eq!(available, 0);
    handle.flush_blocking();
    success.assert_calls(3);
    assert_eq!(handle.pending(), 0);
    assert_eq!(handle.byte_budget.as_ref().unwrap().available(), 64 * 1024);
    assert!(directory.records().is_empty());
    handle.shutdown_blocking();
}

struct ReadFailure(FileEventStore);

impl EventStore for ReadFailure {
    fn recover(&mut self, _: usize, _: u64) -> io::Result<Recovery> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "test read failure",
        ))
    }
    fn put(&mut self, event: &CaptureEvent) -> io::Result<StoreKey> {
        self.0.put(event)
    }
    fn remove(&mut self, key: StoreKey) -> io::Result<()> {
        self.0.remove(key)
    }
}

#[test]
fn failed_recovery_reports_read_error_releases_reservations_and_preserves_disk() {
    let server = MockServer::start();
    let directory = TestDirectory::new();
    let old = wire("old");
    let new = wire("new");
    directory.open().put(&old).unwrap();
    let success = server.mock(|when, then| {
        when.method(POST);
        then.status(200).json_body(serde_json::json!({"results": {
            (old.uuid.to_string()): {"result": "ok"},
            (new.uuid.to_string()): {"result": "ok"}
        }}));
    });
    let (error_tx, error_rx) = mpsc::channel();
    let handle = TransportHandle::spawn_configured(
        options(server.base_url())
            .on_error(move |failure| {
                if let PostHogError::Storage(failure) = failure {
                    error_tx.send(failure.operation()).unwrap();
                }
            })
            .build()
            .unwrap(),
        LaneConfig::analytics(),
        Arc::new(ManualClock::new()),
        StorageState::new(Box::new(ReadFailure(directory.open())), 64 * 1024),
    );
    handle.flush_blocking();
    assert_eq!(
        error_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        StorageOperation::Read
    );
    assert_eq!(handle.pending(), 0);
    assert_eq!(handle.byte_budget.as_ref().unwrap().available(), 64 * 1024);
    assert_eq!(directory.records().len(), 1);
    assert_eq!(handle.enqueue_prepared(new), Admission::Accepted);
    handle.flush_blocking();
    success.assert_calls(1);
    assert_eq!(handle.pending(), 0);
    assert_eq!(directory.records().len(), 1);
    handle.shutdown_blocking();
    let handle = persistent(&server, &ManualClock::new(), directory.open());
    handle.flush_blocking();
    success.assert_calls(2);
    assert!(directory.records().is_empty());
    handle.shutdown_blocking();
}

#[test]
fn corrupt_record_reports_read_error_without_blocking_valid_recovery() {
    let server = MockServer::start();
    let directory = TestDirectory::new();
    let valid = wire("valid");
    {
        let mut store = directory.open();
        store.put(&wire("broken")).unwrap();
        store.put(&valid).unwrap();
    }
    let corrupt_path = directory.records()[0].clone();
    fs::write(&corrupt_path, b"not JSON").unwrap();
    let success = server.mock(|when, then| {
        when.method(POST).body_excludes("broken");
        then.status(200).json_body(serde_json::json!({"results": {
            (valid.uuid.to_string()): {"result": "ok"}
        }}));
    });
    let (error_tx, error_rx) = mpsc::channel();
    let handle = TransportHandle::spawn_persistent(
        options(server.base_url())
            .on_error(move |failure| {
                if let PostHogError::Storage(failure) = failure {
                    error_tx
                        .send((failure.operation(), failure.error().to_string()))
                        .unwrap();
                }
            })
            .build()
            .unwrap(),
        Box::new(directory.open()),
        64 * 1024,
    );
    handle.flush_blocking();
    assert_eq!(
        error_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        (
            StorageOperation::Read,
            "Invalid persisted event record".into()
        )
    );
    success.assert_calls(1);
    assert!(directory.records().is_empty());
    assert!(corrupt_path.with_extension("corrupt").exists());
    assert_eq!(handle.pending(), 0);
    assert_eq!(handle.byte_budget.as_ref().unwrap().available(), 64 * 1024);
    handle.shutdown_blocking();
}

#[test]
fn shutdown_deadline_releases_memory_but_preserves_unacknowledged_chunks() {
    let server = MockServer::start();
    let directory = TestDirectory::new();
    let events: Vec<_> = (0..3)
        .map(|index| wire(&format!("event-{index}")))
        .collect();
    let mut stalled = server.mock(|when, then| {
        when.method(POST);
        then.status(503).delay(Duration::from_millis(200));
    });
    let mut lane = LaneConfig::analytics();
    lane.batch_bytes_target = Some(1); // Each event becomes a separate request.
    let handle = TransportHandle::spawn_configured(
        options(server.base_url())
            .shutdown_timeout_ms(50u64)
            .build()
            .unwrap(),
        lane,
        Arc::new(super::SystemClock),
        StorageState::new(Box::new(directory.open()), 64 * 1024),
    );
    for event in &events {
        assert_eq!(handle.enqueue_prepared(event.clone()), Admission::Accepted);
    }
    handle.shutdown_blocking();
    assert!(
        stalled.calls() <= 1,
        "shutdown must not start subsequent chunks after its deadline"
    );
    assert_eq!(directory.records().len(), 3);
    assert_eq!(handle.pending(), 0);
    assert_eq!(handle.byte_budget.as_ref().unwrap().available(), 64 * 1024);
    stalled.delete();
    let results: serde_json::Map<_, _> = events
        .iter()
        .map(|event| (event.uuid.to_string(), serde_json::json!({"result": "ok"})))
        .collect();
    let success = server.mock(|when, then| {
        when.method(POST);
        then.status(200)
            .json_body(serde_json::json!({"results": results}));
    });
    let handle = persistent(&server, &ManualClock::new(), directory.open());
    handle.flush_blocking();
    success.assert_calls(1);
    assert!(directory.records().is_empty());
    handle.shutdown_blocking();
}

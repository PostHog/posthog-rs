//! Behavior of stateful delivery through the transport handle: a real
//! coordinator and executor against a mock capture server, on an injected
//! clock.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use httpmock::{Method::POST, Mock, MockServer};
use serde_json::{json, Value};
use uuid::Uuid;

use super::super::delivery::Admission;
use super::super::inbox::Inbox;
use super::super::tests::{options, ManualClock};
use super::super::{Clock, Completion, Control, TransportHandle};
use crate::capture_event::CaptureEvent;
use crate::client::event_store::{
    record_size, EventStore, FileEventStore, StorageDirectory, StoreKey, StoreLimits,
};
use crate::client::{ClientOptionsBuilder, PostHogError, StorageOperation};
use crate::Event;

// -- fixtures -----------------------------------------------------------------

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("posthog-delivery-{}", Uuid::now_v7())))
    }

    fn open(&self, max_events: usize) -> FileEventStore {
        FileEventStore::open(
            StorageDirectory::open(&self.0).unwrap(),
            StoreLimits {
                max_events,
                max_bytes: 128 * 1024,
            },
        )
        .unwrap()
    }

    fn records(&self) -> Vec<u64> {
        let mut keys: Vec<u64> = fs::read_dir(self.0.join("events"))
            .unwrap()
            .filter_map(|entry| {
                let path = entry.unwrap().path();
                (path.extension()? == "json")
                    .then(|| path.file_stem()?.to_str()?.parse().ok())
                    .flatten()
            })
            .collect();
        keys.sort_unstable();
        keys
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

/// An event whose disk record measures exactly `bytes`.
fn sized(name: &str, bytes: usize) -> CaptureEvent {
    // A fixed timestamp: its serialized length varies with sub-second precision.
    let timestamp = chrono::Utc::now();
    let padded = |padding: usize| {
        let mut event = Event::new(name, "user");
        event.set_timestamp(timestamp).unwrap();
        event.insert_prop("padding", "x".repeat(padding)).unwrap();
        CaptureEvent::from_event(&event)
    };
    let event = padded(bytes - record_size(&padded(0)));
    assert_eq!(record_size(&event), bytes);
    event
}

/// Storage faults a test can switch on while delivery runs.
#[derive(Clone, Default)]
struct Faults {
    writes: Arc<Mutex<HashSet<String>>>,
    deletes: Arc<AtomicUsize>,
    reads: Arc<Mutex<HashSet<u64>>>,
}

struct FaultyStore {
    inner: FileEventStore,
    faults: Faults,
}

impl EventStore for FaultyStore {
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
        if self.faults.writes.lock().unwrap().contains(&event.event) {
            return Err(io::Error::other("test write"));
        }
        self.inner.write(key, event)
    }
    fn read(&mut self, key: StoreKey) -> io::Result<CaptureEvent> {
        if self.faults.reads.lock().unwrap().contains(&key.0) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "test read"));
        }
        self.inner.read(key)
    }
    fn delete(&mut self, key: StoreKey) -> io::Result<()> {
        if take_one(&self.faults.deletes) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "test delete",
            ));
        }
        self.inner.delete(key)
    }
}

/// Take one from `count` unless it is zero. Rust 1.99 deprecates
/// `fetch_update` for `try_update`, which the 1.78 minimum lacks.
fn take_one(count: &AtomicUsize) -> bool {
    let mut current = count.load(Ordering::Acquire);
    while current > 0 {
        match count.compare_exchange_weak(current, current - 1, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => return true,
            Err(actual) => current = actual,
        }
    }
    false
}

/// One request the mock server received.
#[derive(Clone, Debug)]
struct Received {
    id: String,
    attempt: u32,
    events: Vec<String>,
}

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<Received>>>);

impl Log {
    fn names(&self) -> Vec<Vec<String>> {
        self.all()
            .into_iter()
            .map(|request| request.events)
            .collect()
    }
    fn all(&self) -> Vec<Received> {
        self.0.lock().unwrap().clone()
    }
}

/// Answer every capture request with `status` and `body`, logging it.
fn respond<'a>(server: &'a MockServer, log: &Log, status: u16, body: Value) -> Mock<'a> {
    let log = log.clone();
    server.mock(move |when, then| {
        when.method(POST).is_true(move |request| {
            let header = |name: &str| {
                request
                    .headers_vec()
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value.clone())
                    .unwrap_or_default()
            };
            let body: Value = serde_json::from_slice(request.body_ref()).unwrap_or_default();
            log.0.lock().unwrap().push(Received {
                id: header("posthog-request-id"),
                attempt: header("posthog-attempt").parse().unwrap_or(0),
                events: body["batch"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|event| event["event"].as_str().unwrap_or_default().to_string())
                    .collect(),
            });
            true
        });
        then.status(status).json_body(body);
    })
}

fn delivered<'a>(server: &'a MockServer, log: &Log) -> Mock<'a> {
    respond(server, log, 200, json!({"results": {}}))
}

struct Harness {
    handle: TransportHandle,
    clock: ManualClock,
    deadline: Arc<Mutex<Option<Instant>>>,
}

fn spawn(
    builder: &mut ClientOptionsBuilder,
    store: Option<Box<dyn EventStore>>,
    max_queue_bytes: usize,
) -> Harness {
    let clock = ManualClock::new();
    let deadline = Arc::new(Mutex::new(None));
    let handle = TransportHandle::spawn_stateful_with_deadline(
        builder.build().unwrap(),
        store,
        max_queue_bytes,
        deadline.clone(),
        Arc::new(clock.clone()),
    )
    .unwrap();
    Harness {
        handle,
        clock,
        deadline,
    }
}

fn base(server: &MockServer) -> ClientOptionsBuilder {
    let mut builder = options(server.base_url());
    builder
        .flush_at(100usize)
        .retry_initial_backoff_ms(1_000u64);
    builder
}

fn admit(handle: &TransportHandle, name: &str) {
    assert_eq!(handle.enqueue_prepared(wire(name)), Admission::Accepted);
}

fn flush(handle: &TransportHandle) {
    handle.flush_blocking();
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

fn storage_errors(builder: &mut ClientOptionsBuilder) -> mpsc::Receiver<StorageOperation> {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    builder.on_error(move |error| {
        if let PostHogError::Storage(failure) = error {
            let _ = tx.lock().unwrap().send(failure.operation());
        }
    });
    rx
}

// -- delivery -----------------------------------------------------------------

#[test]
fn memory_only_events_are_delivered_in_order_on_flush() {
    let server = MockServer::start();
    let log = Log::default();
    let _ok = delivered(&server, &log);
    let harness = spawn(&mut base(&server), None, 64 * 1024);
    for name in ["a", "b", "c"] {
        admit(&harness.handle, name);
    }
    assert_eq!(harness.handle.pending(), 3);
    flush(&harness.handle);
    assert_eq!(log.names(), [names(&["a", "b", "c"])]);
    assert_eq!(harness.handle.pending(), 0);
    harness.handle.shutdown_blocking();
}

#[test]
fn persisted_events_leave_memory_and_their_files_go_once_delivered() {
    let server = MockServer::start();
    let log = Log::default();
    let directory = TestDirectory::new();
    let harness = spawn(
        &mut base(&server),
        Some(Box::new(directory.open(10))),
        64 * 1024,
    );
    admit(&harness.handle, "a");
    admit(&harness.handle, "b");
    // Without a response yet, flushing writes both and attempts them.
    let mut offline = respond(&server, &log, 503, json!({}));
    flush(&harness.handle);
    assert_eq!(directory.records(), [1, 2]);
    assert_eq!(
        harness.handle.pending(),
        0,
        "events on disk are not in memory"
    );
    offline.delete();
    let _ok = delivered(&server, &log);
    harness.clock.advance(Duration::from_secs(2));
    harness.handle.tick();
    assert!(directory.records().is_empty());
    harness.handle.shutdown_blocking();
}

#[test]
fn a_backlog_from_an_earlier_run_is_sent_at_start_after_new_keys() {
    let server = MockServer::start();
    let log = Log::default();
    let _ok = delivered(&server, &log);
    let directory = TestDirectory::new();
    {
        let mut store = directory.open(10);
        store.write(StoreKey(7), &wire("earlier")).unwrap();
    }
    let harness = spawn(
        &mut base(&server),
        Some(Box::new(directory.open(10))),
        64 * 1024,
    );
    admit(&harness.handle, "later");
    flush(&harness.handle);
    assert_eq!(log.names().concat(), names(&["earlier", "later"]));
    assert!(directory.records().is_empty());
    harness.handle.shutdown_blocking();
}

#[test]
fn events_absent_from_results_count_as_delivered_and_an_unreadable_body_is_lost() {
    let server = MockServer::start();
    let log = Log::default();
    let directory = TestDirectory::new();
    let failures = Arc::new(AtomicUsize::new(0));
    let observed = failures.clone();
    let mut builder = base(&server);
    builder.on_error(move |error| {
        if matches!(error, PostHogError::Capture(_)) {
            observed.fetch_add(1, Ordering::SeqCst);
        }
    });
    let harness = spawn(&mut builder, Some(Box::new(directory.open(10))), 64 * 1024);
    let mut empty = respond(&server, &log, 200, json!({"results": {}}));
    admit(&harness.handle, "absent");
    flush(&harness.handle);
    empty.delete();
    let proxy = server.mock(|when, then| {
        when.method(POST);
        then.status(200).body("<html>proxy</html>");
    });
    admit(&harness.handle, "lost");
    flush(&harness.handle);
    flush(&harness.handle);
    proxy.assert_calls(1);
    assert!(directory.records().is_empty());
    assert_eq!(failures.load(Ordering::SeqCst), 1);
    harness.handle.shutdown_blocking();
}

/// Read one HTTP request, headers and body, from `stream`.
fn read_request(stream: &mut TcpStream) {
    let mut request = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let read = stream.read(&mut buffer).unwrap();
        if read == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..read]);
        let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
        let length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .map_or(0, |value| value.trim().parse().unwrap());
        if request.len() >= end + 4 + length {
            return;
        }
    }
}

#[test]
fn a_response_whose_body_is_cut_off_is_reported_lost_and_not_retried() {
    for (head, status) in [("HTTP/1.1 200 OK", 200), ("HTTP/1.1 401 Unauthorized", 401)] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            let response = format!("{head}\r\ncontent-length: 100\r\n\r\n{{\"results\"");
            stream.write_all(response.as_bytes()).unwrap();
            let _ = stream.shutdown(Shutdown::Both);
            // A resend would be accepted here; there must be none.
            listener.set_nonblocking(true).unwrap();
            std::thread::sleep(Duration::from_millis(200));
            listener.accept().is_err()
        });
        let directory = TestDirectory::new();
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let observed = statuses.clone();
        let mut builder = options(url);
        builder
            .flush_at(100usize)
            .retry_initial_backoff_ms(1_000u64)
            .on_error(move |error| {
                if let PostHogError::Capture(failure) = error {
                    observed.lock().unwrap().push(failure.status());
                }
            });
        let harness = spawn(&mut builder, Some(Box::new(directory.open(10))), 64 * 1024);
        admit(&harness.handle, "cut-off");
        flush(&harness.handle);
        harness.clock.advance(Duration::from_secs(60));
        harness.handle.tick();
        assert!(
            server.join().unwrap(),
            "status {}: the batch was resent",
            status
        );
        assert_eq!(*statuses.lock().unwrap(), [Some(status)]);
        assert!(
            directory.records().is_empty(),
            "status {}: the event is not kept for a retry",
            status
        );
        harness.handle.request_close_before(harness.clock.now());
        harness.handle.join();
    }
}

/// A capture response body just over the read limit, valid JSON when read
/// in full.
fn oversized_body() -> String {
    let padding = " ".repeat(super::MAX_RESPONSE_BODY_BYTES as usize);
    format!("{{\"results\": {{}}}}{padding}")
}

fn capture_failures(builder: &mut ClientOptionsBuilder) -> Arc<AtomicUsize> {
    let failures = Arc::new(AtomicUsize::new(0));
    let observed = failures.clone();
    builder.on_error(move |error| {
        if matches!(error, PostHogError::Capture(_)) {
            observed.fetch_add(1, Ordering::SeqCst);
        }
    });
    failures
}

#[test]
fn an_oversized_success_body_is_unreadable_reported_lost_and_not_retried() {
    let server = MockServer::start();
    let directory = TestDirectory::new();
    let mut builder = base(&server);
    let failures = capture_failures(&mut builder);
    let harness = spawn(&mut builder, Some(Box::new(directory.open(10))), 64 * 1024);
    let oversized = server.mock(|when, then| {
        when.method(POST);
        then.status(200).body(oversized_body());
    });
    admit(&harness.handle, "lost");
    flush(&harness.handle);
    harness.clock.advance(Duration::from_secs(60));
    harness.handle.tick();
    oversized.assert_calls(1);
    assert!(directory.records().is_empty());
    assert_eq!(failures.load(Ordering::SeqCst), 1);
    harness.handle.shutdown_blocking();
}

#[test]
fn an_oversized_retryable_body_is_still_retried() {
    let server = MockServer::start();
    let directory = TestDirectory::new();
    let mut builder = base(&server);
    let failures = capture_failures(&mut builder);
    let harness = spawn(&mut builder, Some(Box::new(directory.open(10))), 64 * 1024);
    let oversized = server.mock(|when, then| {
        when.method(POST);
        then.status(503).body(oversized_body());
    });
    admit(&harness.handle, "kept");
    flush(&harness.handle);
    harness.clock.advance(Duration::from_secs(60));
    harness.handle.tick();
    oversized.assert_calls(2);
    assert_eq!(directory.records(), [1]);
    assert_eq!(failures.load(Ordering::SeqCst), 0);
    harness.handle.request_close_before(harness.clock.now());
    harness.handle.join();
}

// -- retries ------------------------------------------------------------------

#[test]
fn a_retry_keeps_its_request_identity_and_sends_only_retry_verdicts() {
    let server = MockServer::start();
    let log = Log::default();
    let directory = TestDirectory::new();
    let harness = spawn(
        &mut base(&server),
        Some(Box::new(directory.open(10))),
        64 * 1024,
    );
    let first = wire("first");
    let second = wire("second");
    let mut partial = respond(
        &server,
        &log,
        200,
        json!({"results": {
            (first.uuid.to_string()): {"result": "ok"},
            (second.uuid.to_string()): {"result": "retry"},
        }}),
    );
    assert_eq!(harness.handle.enqueue_prepared(first), Admission::Accepted);
    assert_eq!(harness.handle.enqueue_prepared(second), Admission::Accepted);
    flush(&harness.handle);
    assert_eq!(directory.records(), [2]);
    partial.delete();
    let _ok = delivered(&server, &log);
    // Not yet due: a flush does not bypass the retry wait.
    flush(&harness.handle);
    assert_eq!(log.all().len(), 1);
    harness.clock.advance(Duration::from_secs(2));
    harness.handle.tick();
    let requests = log.all();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].events, names(&["second"]));
    assert_eq!(requests[1].id, requests[0].id);
    assert_eq!(requests[1].attempt, 2);
    assert!(directory.records().is_empty());
    harness.handle.shutdown_blocking();
}

#[test]
fn a_retry_whose_events_left_the_queue_starts_a_new_request() {
    let server = MockServer::start();
    let log = Log::default();
    let directory = TestDirectory::new();
    let harness = spawn(
        &mut base(&server),
        Some(Box::new(directory.open(2))),
        64 * 1024,
    );
    let mut offline = respond(&server, &log, 503, json!({}));
    admit(&harness.handle, "a");
    admit(&harness.handle, "b");
    flush(&harness.handle);
    // A third event evicts "a" from the full disk queue.
    admit(&harness.handle, "c");
    flush(&harness.handle);
    assert_eq!(directory.records(), [2, 3]);
    offline.delete();
    let _ok = delivered(&server, &log);
    harness.clock.advance(Duration::from_secs(2));
    harness.handle.tick();
    let requests = log.all();
    let last = requests.last().unwrap();
    assert_eq!(last.events, names(&["b", "c"]));
    assert_ne!(last.id, requests[0].id);
    assert_eq!(last.attempt, 1);
    harness.handle.shutdown_blocking();
}

/// Drive `rounds` 1.3 s ticks against an always-503 server, admitting one new
/// event before each tick, and return the attempts the server saw.
fn backoff_attempts(max_queue_size: usize, rounds: usize) -> Vec<u32> {
    let server = MockServer::start();
    let log = Log::default();
    let mut builder = base(&server);
    builder.max_queue_size(max_queue_size);
    let harness = spawn(&mut builder, None, 64 * 1024);
    let _offline = respond(&server, &log, 503, json!({}));
    admit(&harness.handle, "a");
    admit(&harness.handle, "b");
    flush(&harness.handle);
    for round in 0..rounds {
        admit(&harness.handle, &format!("new-{}", round));
        // Past the longest first wait (1 s x 1.25 jitter), short of the
        // shortest second wait (2 s x 0.75 jitter).
        harness.clock.advance(Duration::from_millis(1_300));
        harness.handle.tick();
    }
    let attempts = log.all().iter().map(|request| request.attempt).collect();
    harness.handle.request_close_before(harness.clock.now());
    harness.handle.join();
    attempts
}

#[test]
fn the_backoff_keeps_growing_when_capacity_evicts_a_retried_event() {
    // Without eviction the waits double: fewer than five requests.
    assert!(backoff_attempts(100, 4).len() < 5);
    // Eviction drops the retry identity (new attempt-1 requests) but must
    // not reset the wait.
    let attempts = backoff_attempts(2, 4);
    assert!(attempts.len() < 5, "the backoff reset: {:?}", attempts);
    assert!(
        attempts.iter().all(|attempt| *attempt == 1),
        "{:?}",
        attempts
    );
}

#[test]
fn delivery_keeps_retrying_past_max_capture_attempts_with_or_without_a_store() {
    for persistent in [false, true] {
        let server = MockServer::start();
        let log = Log::default();
        let directory = TestDirectory::new();
        let failures = Arc::new(AtomicUsize::new(0));
        let observed = failures.clone();
        let mut builder = base(&server);
        builder.max_capture_attempts(2u32).on_error(move |error| {
            if matches!(error, PostHogError::Capture(_)) {
                observed.fetch_add(1, Ordering::SeqCst);
            }
        });
        let store: Option<Box<dyn EventStore>> =
            persistent.then(|| Box::new(directory.open(10)) as Box<dyn EventStore>);
        let harness = spawn(&mut builder, store, 64 * 1024);
        let _offline = respond(&server, &log, 503, json!({}));
        admit(&harness.handle, "event");
        flush(&harness.handle);
        for _ in 0..3 {
            harness.clock.advance(Duration::from_secs(60));
            harness.handle.tick();
        }
        assert_eq!(log.all().len(), 4);
        assert_eq!(failures.load(Ordering::SeqCst), 0);
        if persistent {
            assert_eq!(directory.records(), [1]);
        }
        harness.handle.request_close_before(harness.clock.now());
        harness.handle.join();
    }
}

// -- storage faults -----------------------------------------------------------

#[test]
fn a_failed_delete_is_reported_and_leaves_the_file_for_the_next_start() {
    let server = MockServer::start();
    let log = Log::default();
    let _ok = delivered(&server, &log);
    let directory = TestDirectory::new();
    let faults = Faults::default();
    faults.deletes.store(1, Ordering::Release);
    let mut builder = base(&server);
    let errors = storage_errors(&mut builder);
    let harness = spawn(
        &mut builder,
        Some(Box::new(FaultyStore {
            inner: directory.open(10),
            faults: faults.clone(),
        })),
        64 * 1024,
    );
    admit(&harness.handle, "first");
    flush(&harness.handle);
    assert_eq!(
        errors.recv_timeout(Duration::from_secs(5)).unwrap(),
        StorageOperation::Remove
    );
    admit(&harness.handle, "second");
    flush(&harness.handle);
    assert_eq!(log.names(), [names(&["first"]), names(&["second"])]);
    assert_eq!(directory.records(), [1], "the file is not deleted again");
    harness.handle.shutdown_blocking();
}

#[test]
fn an_unreadable_record_is_reported_dropped_and_deleted() {
    let server = MockServer::start();
    let log = Log::default();
    let _ok = delivered(&server, &log);
    let directory = TestDirectory::new();
    {
        let mut store = directory.open(10);
        store.write(StoreKey(1), &wire("unreadable")).unwrap();
    }
    let faults = Faults::default();
    faults.reads.lock().unwrap().insert(1);
    let mut builder = base(&server);
    let errors = storage_errors(&mut builder);
    let harness = spawn(
        &mut builder,
        Some(Box::new(FaultyStore {
            inner: directory.open(10),
            faults,
        })),
        64 * 1024,
    );
    admit(&harness.handle, "fresh");
    flush(&harness.handle);
    assert_eq!(log.names().concat(), names(&["fresh"]));
    assert_eq!(
        errors.try_iter().collect::<Vec<_>>(),
        [StorageOperation::Read]
    );
    assert!(directory.records().is_empty());
    harness.handle.shutdown_blocking();
}

#[test]
fn a_retried_event_that_becomes_unreadable_is_dropped_and_the_rest_go_in_a_new_request() {
    let server = MockServer::start();
    let log = Log::default();
    let directory = TestDirectory::new();
    let faults = Faults::default();
    let harness = spawn(
        &mut base(&server),
        Some(Box::new(FaultyStore {
            inner: directory.open(10),
            faults: faults.clone(),
        })),
        64 * 1024,
    );
    let mut offline = respond(&server, &log, 503, json!({}));
    admit(&harness.handle, "a");
    admit(&harness.handle, "b");
    flush(&harness.handle);
    offline.delete();
    let _ok = delivered(&server, &log);
    faults.reads.lock().unwrap().insert(1);
    harness.clock.advance(Duration::from_secs(2));
    harness.handle.tick();
    let requests = log.all();
    assert_eq!(log.names(), [names(&["a", "b"]), names(&["b"])]);
    assert_ne!(requests[1].id, requests[0].id);
    assert_eq!(requests[1].attempt, 1);
    assert!(directory.records().is_empty());
    harness.handle.shutdown_blocking();
}

#[test]
fn an_emptied_queue_keeps_no_due_time_for_the_next_event() {
    let directory = TestDirectory::new();
    directory
        .open(10)
        .write(StoreKey(1), &wire("unreadable"))
        .unwrap();
    let faults = Faults::default();
    faults.reads.lock().unwrap().insert(1);
    let clock = ManualClock::new();
    let store = FaultyStore {
        inner: directory.open(10),
        faults,
    };
    let (mut coordinator, inbox, _pending) = idle_coordinator(Box::new(store), &clock, None);
    let (jobs, _sent) = mpsc::sync_channel(1);
    coordinator.jobs = Some(jobs);
    // The backlog's only record is unreadable, so it is dropped.
    assert!(coordinator.start_request());
    assert!(coordinator.table.is_empty());
    clock.advance(Duration::from_secs(5));
    assert_eq!(inbox.admit(wire("newer"), 10), Admission::Accepted);
    coordinator.drain_inbox();
    assert!(coordinator.start_request());
    assert!(
        !coordinator.sending,
        "the new event waits for its flush interval"
    );
}

#[test]
fn a_failed_write_drops_the_event_and_reports_the_failure_once() {
    let server = MockServer::start();
    let log = Log::default();
    let _ok = delivered(&server, &log);
    let directory = TestDirectory::new();
    let faults = Faults::default();
    faults
        .writes
        .lock()
        .unwrap()
        .extend(["b".to_string(), "c".to_string()]);
    let mut builder = base(&server);
    let errors = storage_errors(&mut builder);
    let harness = spawn(
        &mut builder,
        Some(Box::new(FaultyStore {
            inner: directory.open(10),
            faults,
        })),
        64 * 1024,
    );
    for name in ["a", "b", "c", "d"] {
        admit(&harness.handle, name);
    }
    flush(&harness.handle);
    assert_eq!(log.names(), [names(&["a", "d"])]);
    assert_eq!(
        errors.try_iter().collect::<Vec<_>>(),
        [StorageOperation::Write]
    );
    assert_eq!(harness.handle.pending(), 0);
    harness.handle.shutdown_blocking();
}

// -- capacity -----------------------------------------------------------------

#[test]
fn an_event_larger_than_the_queue_byte_limit_is_accepted_and_sent_alone() {
    let server = MockServer::start();
    let log = Log::default();
    let _ok = delivered(&server, &log);
    let harness = spawn(&mut base(&server), None, 1024);
    admit(&harness.handle, "small");
    assert_eq!(
        harness.handle.enqueue_prepared(sized("large", 2048)),
        Admission::Accepted
    );
    flush(&harness.handle);
    // The newest event is kept even when it alone fills the queue.
    assert_eq!(log.names(), [names(&["large"])]);
    harness.handle.shutdown_blocking();
}

#[test]
fn a_full_memory_queue_keeps_the_newest_events() {
    let server = MockServer::start();
    let log = Log::default();
    let mut builder = base(&server);
    builder.max_queue_size(2usize);
    let harness = spawn(&mut builder, None, 64 * 1024);
    let mut offline = respond(&server, &log, 503, json!({}));
    for name in ["a", "b", "c"] {
        admit(&harness.handle, name);
    }
    flush(&harness.handle);
    assert_eq!(harness.handle.pending(), 2);
    offline.delete();
    let _ok = delivered(&server, &log);
    harness.clock.advance(Duration::from_secs(60));
    harness.handle.tick();
    assert_eq!(log.names().last().unwrap(), &names(&["b", "c"]));
    harness.handle.shutdown_blocking();
}

// -- shutdown -----------------------------------------------------------------

#[test]
fn shutdown_waits_for_a_retry_that_falls_before_its_deadline() {
    let server = MockServer::start();
    let log = Log::default();
    let mut builder = base(&server);
    builder.shutdown_timeout_ms(60_000u64);
    let harness = spawn(&mut builder, None, 64 * 1024);
    let mut offline = respond(&server, &log, 503, json!({}));
    admit(&harness.handle, "a");
    flush(&harness.handle);
    offline.delete();
    let _ok = delivered(&server, &log);
    let (done, finished) = mpsc::channel();
    assert!(harness.handle.request_shutdown(Completion::Blocking(done)));
    // The retry is due within the shutdown budget, so shutdown waits for it.
    assert!(finished.recv_timeout(Duration::from_millis(200)).is_err());
    harness.clock.advance(Duration::from_secs(2));
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(log.names().last().unwrap(), &names(&["a"]));
    harness.handle.join();
    assert!(harness.handle.all_stopped());
}

#[test]
fn shutdown_stops_at_a_retry_beyond_its_deadline_and_keeps_disk_events() {
    let server = MockServer::start();
    let log = Log::default();
    let directory = TestDirectory::new();
    let harness = spawn(
        &mut base(&server),
        Some(Box::new(directory.open(10))),
        64 * 1024,
    );
    let _offline = respond(&server, &log, 503, json!({"error": "down"}));
    admit(&harness.handle, "kept");
    flush(&harness.handle);
    *harness.deadline.lock().unwrap() = Some(harness.clock.now() + Duration::from_millis(1));
    harness.handle.shutdown_blocking();
    assert!(harness.handle.all_stopped());
    assert_eq!(log.all().len(), 1);
    assert_eq!(directory.records(), [1]);
    // The lease is released, so the next start finds the event.
    drop(directory.open(10));
}

#[test]
fn a_dropped_handle_stops_its_threads_without_a_shutdown() {
    let server = MockServer::start();
    let clock = Arc::new(ManualClock::new());
    let handle = TransportHandle::spawn_stateful_with_deadline(
        base(&server).build().unwrap(),
        None,
        64 * 1024,
        Arc::new(Mutex::new(None)),
        clock.clone(),
    )
    .unwrap();
    drop(handle);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Arc::strong_count(&clock) > 1 {
        assert!(
            Instant::now() < deadline,
            "a delivery thread is still running"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_closed_handle_refuses_admission_and_flushes_complete() {
    let server = MockServer::start();
    let harness = spawn(&mut base(&server), None, 64 * 1024);
    harness.handle.shutdown_blocking();
    assert_eq!(
        harness.handle.enqueue_prepared(wire("late")),
        Admission::Closed
    );
    let (done, finished) = mpsc::channel();
    // Delivery has stopped, so the flush is either refused or completed.
    if harness
        .handle
        .send_control(Control::Flush(Completion::Blocking(done)))
    {
        finished.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}

// -- regressions --------------------------------------------------------------

#[test]
fn events_admitted_after_the_deadline_are_still_written_to_disk() {
    let directory = TestDirectory::new();
    let clock = ManualClock::new();
    // The deadline has already passed when the event is admitted, before the
    // coordinator's final drain.
    let (coordinator, inbox, pending) =
        idle_coordinator(Box::new(directory.open(10)), &clock, Some(clock.now()));
    let last = wire("last");
    let bytes = record_size(&last);
    assert_eq!(inbox.admit(last, bytes), Admission::Accepted);
    let (jobs, _jobs) = mpsc::sync_channel(1);
    let (_control, rx) = mpsc::channel();
    coordinator.run(jobs, rx);
    assert_eq!(directory.records(), [1]);
    assert_eq!(pending.load(Ordering::Acquire), 0);
}

#[test]
fn a_full_disk_queue_stopping_at_its_deadline_keeps_the_newest_events_within_its_limits() {
    let directory = TestDirectory::new();
    {
        let mut store = directory.open(2);
        store.write(StoreKey(1), &wire("a")).unwrap();
        store.write(StoreKey(2), &wire("b")).unwrap();
    }
    let clock = ManualClock::new();
    let (coordinator, inbox, pending) =
        idle_coordinator(Box::new(directory.open(2)), &clock, Some(clock.now()));
    for name in ["c", "d"] {
        let event = wire(name);
        let bytes = record_size(&event);
        assert_eq!(inbox.admit(event, bytes), Admission::Accepted);
    }
    let (jobs, _jobs) = mpsc::sync_channel(1);
    let (_control, rx) = mpsc::channel();
    // Past its deadline, the coordinator stops at once.
    coordinator.run(jobs, rx);
    assert_eq!(directory.records(), [3, 4]);
    assert_eq!(pending.load(Ordering::Acquire), 0);
}

#[test]
fn an_admission_after_the_final_drain_is_refused() {
    let directory = TestDirectory::new();
    let clock = ManualClock::new();
    let (coordinator, inbox, pending) =
        idle_coordinator(Box::new(directory.open(10)), &clock, Some(clock.now()));
    let (jobs, _jobs) = mpsc::sync_channel(1);
    let (_control, rx) = mpsc::channel();
    coordinator.run(jobs, rx);
    let late = wire("late");
    let bytes = record_size(&late);
    assert_eq!(inbox.admit(late, bytes), Admission::Closed);
    assert_eq!(pending.load(Ordering::Acquire), 0);
}

#[test]
fn new_events_never_reuse_the_key_of_a_record_left_on_disk() {
    let server = MockServer::start();
    let log = Log::default();
    let directory = TestDirectory::new();
    directory
        .open(10)
        .write(StoreKey(2), &wire("earlier"))
        .unwrap();
    let _offline = respond(&server, &log, 503, json!({}));
    let harness = spawn(
        &mut base(&server),
        Some(Box::new(directory.open(10))),
        64 * 1024,
    );
    admit(&harness.handle, "new");
    flush(&harness.handle);
    assert_eq!(directory.records(), [2, 3]);
    harness.handle.request_close_before(harness.clock.now());
    harness.handle.join();
}

#[test]
fn duplicate_copies_dropped_before_a_retry_are_reported_with_its_outcome() {
    let server = MockServer::start();
    let log = Log::default();
    let reported = Arc::new(Mutex::new(Vec::new()));
    let observed = reported.clone();
    let mut builder = base(&server);
    builder.on_error(move |error| {
        if let PostHogError::Capture(failure) = error {
            observed.lock().unwrap().push(failure.event_count());
        }
    });
    let harness = spawn(&mut builder, None, 64 * 1024);
    let original = wire("copied");
    let uuid = original.uuid.to_string();
    let mut retry = respond(
        &server,
        &log,
        200,
        json!({"results": {(uuid.clone()): {"result": "retry", "duplicates_dropped": 2}}}),
    );
    for _ in 0..3 {
        assert_eq!(
            harness.handle.enqueue_prepared(original.clone()),
            Admission::Accepted
        );
    }
    flush(&harness.handle);
    retry.delete();
    let _ok = respond(
        &server,
        &log,
        200,
        json!({"results": {(uuid): {"result": "ok"}}}),
    );
    harness.clock.advance(Duration::from_secs(2));
    harness.handle.tick();
    assert_eq!(log.all()[1].events, names(&["copied"]));
    assert_eq!(*reported.lock().unwrap(), [2]);
    harness.handle.shutdown_blocking();
}

#[test]
fn a_slow_on_error_hook_never_holds_up_persistence() {
    let server = MockServer::start();
    let log = Log::default();
    let _rejected = respond(&server, &log, 400, json!({}));
    let directory = TestDirectory::new();
    let (entered, waiting) = mpsc::channel();
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let mut builder = base(&server);
    builder.on_error(move |error| {
        if matches!(error, PostHogError::Capture(_)) {
            let _ = entered.send(());
            let _ = gate.lock().unwrap().recv_timeout(Duration::from_secs(5));
        }
    });
    let harness = spawn(&mut builder, Some(Box::new(directory.open(10))), 64 * 1024);
    let handle = Arc::new(harness.handle);
    admit(&handle, "rejected");
    let flushing = handle.clone();
    let flusher = std::thread::spawn(move || flushing.flush_blocking());
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    // The hook is running; a new event still reaches disk.
    admit(&handle, "written");
    let deadline = Instant::now() + Duration::from_secs(5);
    while directory.records() != [1, 2] {
        assert!(Instant::now() < deadline, "{:?}", directory.records());
        std::thread::sleep(Duration::from_millis(5));
    }
    release.send(()).unwrap();
    flusher.join().unwrap();
    handle.request_close_before(harness.clock.now());
    handle.join();
}

#[test]
fn a_flush_completes_once_its_events_are_attempted_despite_newer_traffic() {
    let server = MockServer::start();
    let slot: Arc<Mutex<Option<Arc<TransportHandle>>>> = Arc::new(Mutex::new(None));
    let requests = Arc::new(AtomicUsize::new(0));
    let _ok = {
        let slot = slot.clone();
        let requests = requests.clone();
        server.mock(move |when, then| {
            when.method(POST).is_true(move |_| {
                // Every request brings a newer event while it is out.
                if requests.fetch_add(1, Ordering::SeqCst) < 30 {
                    if let Some(handle) = slot.lock().unwrap().as_ref() {
                        let _ = handle.enqueue_prepared(wire("newer"));
                    }
                }
                true
            });
            then.status(200).json_body(json!({"results": {}}));
        })
    };
    let mut builder = base(&server);
    builder.flush_at(1usize);
    let harness = spawn(&mut builder, None, 64 * 1024);
    let handle = Arc::new(harness.handle);
    *slot.lock().unwrap() = Some(handle.clone());
    admit(&handle, "first");
    flush(&handle);
    let at_flush = requests.load(Ordering::SeqCst);
    *slot.lock().unwrap() = None;
    handle.request_close_before(harness.clock.now());
    handle.join();
    assert!(
        at_flush < 10,
        "the flush waited for {} requests of newer events",
        at_flush
    );
}

#[test]
fn events_admitted_during_a_send_wait_for_their_own_flush_interval() {
    let server = MockServer::start();
    let log = Log::default();
    let slot: Arc<Mutex<Option<Arc<TransportHandle>>>> = Arc::new(Mutex::new(None));
    let _ok = {
        let slot = slot.clone();
        let log = log.clone();
        server.mock(move |when, then| {
            when.method(POST).is_true(move |request| {
                let body: Value = serde_json::from_slice(request.body_ref()).unwrap_or_default();
                let mut requests = log.0.lock().unwrap();
                if requests.is_empty() {
                    if let Some(handle) = slot.lock().unwrap().as_ref() {
                        let _ = handle.enqueue_prepared(wire("newer"));
                    }
                }
                requests.push(Received {
                    id: String::new(),
                    attempt: 0,
                    events: body["batch"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|event| event["event"].as_str().unwrap_or_default().to_string())
                        .collect(),
                });
                true
            });
            then.status(200).json_body(json!({"results": {}}));
        })
    };
    let harness = spawn(&mut base(&server), None, 64 * 1024);
    let handle = Arc::new(harness.handle);
    *slot.lock().unwrap() = Some(handle.clone());
    admit(&handle, "first");
    handle.tick();
    // The clock stands still, so the newer event's interval never ends.
    std::thread::sleep(Duration::from_millis(300));
    *slot.lock().unwrap() = None;
    handle.request_close_before(harness.clock.now());
    handle.join();
    assert_eq!(
        log.names(),
        [names(&["first"])],
        "a newer event skipped its flush interval"
    );
}

#[test]
fn events_admitted_during_a_retry_wait_go_in_their_own_request_once_it_clears() {
    let server = MockServer::start();
    let log = Log::default();
    let harness = spawn(&mut base(&server), None, 64 * 1024);
    let mut offline = respond(&server, &log, 503, json!({}));
    admit(&harness.handle, "a");
    flush(&harness.handle);
    offline.delete();
    let _ok = delivered(&server, &log);
    admit(&harness.handle, "b");
    harness.clock.advance(Duration::from_secs(2));
    // Wake the coordinator with a flush that covers only "a", so nothing but
    // the cleared retry sends "b" before its interval.
    let (done, finished) = mpsc::channel();
    assert!(harness.handle.send_control(Control::QueueFlush {
        completion: Completion::Blocking(done),
        through: 1,
    }));
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while log.all().len() < 3 {
        assert!(Instant::now() < deadline, "{:?}", log.names());
        std::thread::sleep(Duration::from_millis(5));
    }
    let requests = log.all();
    assert_eq!(log.names(), [names(&["a"]), names(&["a"]), names(&["b"])]);
    assert_eq!(requests[1].id, requests[0].id);
    assert_eq!(requests[1].attempt, 2);
    assert_ne!(requests[2].id, requests[0].id);
    assert_eq!(requests[2].attempt, 1);
    harness.handle.shutdown_blocking();
}

/// A coordinator over `store` that is not running yet, with its inbox and
/// pending count.
fn idle_coordinator(
    store: Box<dyn EventStore>,
    clock: &ManualClock,
    deadline: Option<Instant>,
) -> (super::Coordinator, Arc<Inbox>, Arc<AtomicUsize>) {
    use super::super::table::Limits;
    use super::{Coordinator, Settings};
    let options = options("http://127.0.0.1:1".into()).build().unwrap();
    let pending = Arc::new(AtomicUsize::new(0));
    let (coordinator, inbox) = Coordinator::new(
        Settings {
            lane: super::super::LaneConfig::analytics(),
            memory: Limits {
                events: 10,
                bytes: 64 * 1024,
            },
            options,
        },
        Some(store),
        Arc::new(clock.clone()),
        pending.clone(),
        Arc::new(Mutex::new(deadline)),
    );
    (coordinator, inbox, pending)
}

fn read_fault_coordinator(
    records: &[(u64, &str)],
    faults: &Faults,
    directory: &TestDirectory,
    clock: &ManualClock,
) -> super::Coordinator {
    {
        let mut store = directory.open(10);
        for (key, name) in records {
            store.write(StoreKey(*key), &wire(name)).unwrap();
        }
    }
    let store = FaultyStore {
        inner: directory.open(10),
        faults: faults.clone(),
    };
    idle_coordinator(Box::new(store), clock, None).0
}

#[test]
fn a_retry_answer_for_an_event_evicted_in_flight_keeps_no_duplicate_count() {
    use super::super::table::Limits;
    let directory = TestDirectory::new();
    let clock = ManualClock::new();
    let mut coordinator = read_fault_coordinator(
        &[(1, "evicted"), (2, "kept")],
        &Faults::default(),
        &directory,
        &clock,
    );
    let (jobs, _sent) = mpsc::sync_channel(1);
    coordinator.jobs = Some(jobs);
    assert!(coordinator.start_request());
    assert!(coordinator.sending);
    // Capacity evicts the oldest event while its request is out.
    let evicted = coordinator.table.evict(Limits {
        events: 1,
        bytes: u64::MAX,
    });
    coordinator.discard(evicted);
    coordinator.apply(super::Sent {
        seqs: vec![1, 2],
        request_id: Uuid::now_v7(),
        attempt: 1,
        created_at: String::new(),
        verdict: super::Verdict::Retry {
            retained: vec![(1, 2), (2, 3)],
            retry_after: None,
        },
    });
    assert_eq!(coordinator.copies.keys().copied().collect::<Vec<_>>(), [2]);
}

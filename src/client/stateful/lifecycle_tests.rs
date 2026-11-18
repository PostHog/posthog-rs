use super::*;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread::{self, ThreadId};
use std::time::Duration;

use crate::client::event_store::StorageDirectory;
use crate::client::stateful::storage::{FileStateStore, SaveOutcome, StateStore};
use crate::client::transport::Admission;
use crate::{Event, PostHogError};
use httpmock::{Method::POST, MockServer};
use uuid::Uuid;

#[cfg(feature = "async-client")]
#[path = "drain_lifecycle_tests.rs"]
mod drains;

const WAIT: Duration = Duration::from_secs(5);
const GATE_WAIT: Duration = Duration::from_secs(15);

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("posthog-client-lifecycle-{}", Uuid::now_v7())))
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct GatedStore {
    inner: Option<FileStateStore>,
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    dropped: mpsc::Sender<ThreadId>,
}

impl StateStore for GatedStore {
    fn save(&mut self, snapshot: &Snapshot) -> io::Result<SaveOutcome> {
        self.entered.send(()).unwrap();
        self.release
            .recv_timeout(GATE_WAIT)
            .map_err(|_| io::Error::other("test write gate was not released"))?;
        self.inner.as_mut().unwrap().save(snapshot)
    }
}

impl Drop for GatedStore {
    fn drop(&mut self) {
        drop(self.inner.take());
        let _ = self.dropped.send(thread::current().id());
    }
}

struct WriteGate {
    entered: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    dropped: mpsc::Receiver<ThreadId>,
}

fn gated_writer(directory: &Directory) -> (StateWriter, WriteGate) {
    let inner = FileStateStore::open(StorageDirectory::open(&directory.0).unwrap())
        .unwrap()
        .store;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    let state = StateWriter::new(
        Snapshot::anonymous("anonymous-test-id".into()),
        Backend::Persistent {
            store: Box::new(GatedStore {
                inner: Some(inner),
                entered: entered_tx,
                release: release_rx,
                dropped: dropped_tx,
            }),
            restored: false,
        },
        WriterOptions {
            shutdown_timeout: Duration::ZERO,
            ..WriterOptions::default()
        },
        vec![],
        Arc::new(SystemClock),
    )
    .unwrap();
    (
        state,
        WriteGate {
            entered: entered_rx,
            release: release_tx,
            dropped: dropped_rx,
        },
    )
}

fn config() -> Config {
    let mut config = Config::new("phc_test");
    config.host = "http://127.0.0.1:1".into();
    config.persistence = Persistence::Memory;
    config.shutdown_timeout = Duration::ZERO;
    config
}

fn client_with_writer(config: &Config, state: StateWriter) -> Client {
    client_with_store(config, state, None)
}

fn client_with_store(
    config: &Config,
    state: StateWriter,
    store: Option<Box<dyn crate::client::event_store::EventStore>>,
) -> Client {
    let deadline = state.deadline_handle();
    let shutdown = super::shutdown::Shutdown::new_with_deadline(deadline.clone()).unwrap();
    Client {
        disabled: false,
        shutdown_timeout: config.shutdown_timeout,
        shutdown: Some(shutdown),
        producer: Some(Producer::new(config)),
        flag_loader: Some(Arc::new(super::super::flags::FlagLoader::new(
            config.transport_options(),
            config.on_feature_flags.clone(),
        ))),
        state: Some(state),
        transport: Some(
            TransportHandle::spawn_stateful_with_deadline(
                config.transport_options(),
                store,
                config.max_queue_bytes,
                deadline,
                Arc::new(SystemClock),
            )
            .unwrap(),
        ),
        closed: AtomicBool::new(false),
    }
}

#[test]
fn application_capture_destructor_runs_outside_sdk_state_locks_after_workers_stop() {
    struct CaptureDrop {
        access: Arc<Mutex<Option<super::super::writer::StateAccess>>>,
        entered: mpsc::Sender<ThreadId>,
        release: mpsc::Receiver<()>,
    }
    impl CaptureDrop {
        fn retain(&self) {}
    }
    impl Drop for CaptureDrop {
        fn drop(&mut self) {
            let access = self.access.lock().unwrap().take();
            if let Some(access) = access {
                assert!(access.snapshot().is_none());
            }
            self.entered.send(thread::current().id()).unwrap();
            self.release.recv_timeout(GATE_WAIT).unwrap();
        }
    }
    let (entered, waiting) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let access = Arc::new(Mutex::new(None));
    let captured = CaptureDrop {
        access: access.clone(),
        entered,
        release: gate,
    };
    let mut config = config();
    config.preload_feature_flags = false;
    config.before_send(move |event| {
        captured.retain();
        Some(event)
    });
    let client = Client::init_blocking(config);
    *access.lock().unwrap() = Some(client.state.as_ref().unwrap().access());
    assert_eq!(
        client
            .shutdown_blocking(Some(Duration::from_secs(2)))
            .delivery,
        DeliveryOutcome::Stopped
    );
    let (returned, done) = mpsc::channel();
    let dropper = thread::spawn(move || {
        drop(client);
        returned.send(()).unwrap();
    });
    let caller = dropper.thread().id();
    let destructor = waiting.recv_timeout(WAIT).unwrap();
    let waiting_for_application = done.try_recv().is_err();
    release.send(()).unwrap();
    dropper.join().unwrap();
    eprintln!(
        "application-destructor on_drop_caller={} caller_waiting_for_application={}",
        destructor == caller,
        waiting_for_application
    );
}

#[test]
fn shutdown_does_not_wait_for_a_contended_state_mutation() {
    let mut config = config();
    config.shutdown_timeout = Duration::from_millis(20);
    let state = StateWriter::new(
        Snapshot::anonymous("anonymous".into()),
        Backend::Memory,
        WriterOptions::default(),
        vec![],
        Arc::new(SystemClock),
    )
    .unwrap();
    let client = client_with_writer(&config, state);
    let access = client.state.as_ref().unwrap().access();
    let (entered, waiting) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let mutation = thread::spawn(move || {
        access.mutate(|_| {
            entered.send(()).unwrap();
            gate.recv_timeout(GATE_WAIT).unwrap();
        })
    });
    waiting.recv_timeout(WAIT).unwrap();
    let start = Instant::now();
    let report = client.shutdown_blocking(None);
    drop(client);
    let elapsed = start.elapsed();
    release.send(()).unwrap();
    mutation.join().unwrap().unwrap();
    assert_eq!(report.state, PersistenceOutcome::MemoryOnly);
    assert!(
        elapsed < Duration::from_secs(1),
        "shutdown waited for state mutation: {:?}",
        elapsed
    );
}

#[cfg(feature = "async-client")]
#[test]
fn cancellation_after_readiness_notification_never_drops_client_on_cancelling_thread() {
    use std::future::Future;
    use std::task::{Context, Poll};

    struct ReadyWake(mpsc::Sender<()>);
    impl futures::task::ArcWake for ReadyWake {
        fn wake_by_ref(waker: &Arc<Self>) {
            let _ = waker.0.send(());
        }
    }
    let directory = Directory::new();
    let (state, gate) = gated_writer(&directory);
    let (initialize_tx, initialize_rx) = mpsc::channel();
    let mut future = Box::pin(Client::init_with(Arc::new(config()), move |config| {
        initialize_rx.recv_timeout(GATE_WAIT).unwrap();
        client_with_writer(&config, state)
    }));
    let (ready_tx, ready_rx) = mpsc::channel();
    let waker = futures::task::waker(Arc::new(ReadyWake(ready_tx)));
    let mut context = Context::from_waker(&waker);
    assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
    initialize_tx.send(()).unwrap();
    gate.entered.recv_timeout(WAIT).unwrap();
    // Completion was sent, but the future has not been polled to claim Client.
    ready_rx.recv_timeout(WAIT).unwrap();
    let (cancelled_tx, cancelled_rx) = mpsc::channel();
    let cancelling = thread::spawn(move || {
        drop(future);
        cancelled_tx.send(thread::current().id()).unwrap();
    });
    let cancelled_before_release = cancelled_rx.recv_timeout(WAIT);
    let lease_retained = StorageDirectory::open(&directory.0).is_err();
    gate.release.send(()).unwrap();
    let cleanup_thread = gate.dropped.recv_timeout(WAIT).unwrap();
    cancelling.join().unwrap();
    assert!(lease_retained);
    assert!(
        cancelled_before_release.is_ok(),
        "cancellation waited for state I/O"
    );
    assert_ne!(cleanup_thread, cancelled_before_release.unwrap());
    drop(StorageDirectory::open(&directory.0).unwrap());
}

#[test]
fn final_client_drop_in_delivery_callback_does_not_join_gated_state_worker() {
    let directory = Directory::new();
    let (state, gate) = gated_writer(&directory);
    gate.entered.recv_timeout(WAIT).unwrap();
    let server = MockServer::start();
    let reject = server.mock(|when, then| {
        when.method(POST);
        then.status(400);
    });
    let holder = Arc::new(Mutex::new(None::<Arc<Client>>));
    let callback_holder = holder.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let (dropped_tx, dropped_rx) = mpsc::channel();
    let mut config = config();
    config.host = server.base_url();
    config.flush_at = 1;
    config.on_error(move |failure| {
        if matches!(failure, PostHogError::Capture(_)) {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv_timeout(GATE_WAIT).unwrap();
            // Only this callback's holder still owns the client. Its guard is
            // deliberately held while the client's fields are destroyed.
            let mut holder = callback_holder.lock().unwrap();
            drop(holder.take().unwrap());
            dropped_tx.send(()).unwrap();
        }
    });
    let client = Arc::new(client_with_writer(&config, state));
    *holder.lock().unwrap() = Some(client.clone());
    assert_eq!(
        client.transport.as_ref().unwrap().enqueue_prepared(
            crate::capture_event::CaptureEvent::from_event(&Event::new("rejected", "anon"))
        ),
        Admission::Accepted
    );
    entered_rx.recv_timeout(WAIT).unwrap();
    drop(client);
    release_tx.send(()).unwrap();
    let dropped_before_release = dropped_rx.recv_timeout(WAIT);
    let lease_retained = StorageDirectory::open(&directory.0).is_err();
    gate.release.send(()).unwrap();
    gate.dropped.recv_timeout(WAIT).unwrap();
    if dropped_before_release.is_err() {
        dropped_rx.recv_timeout(WAIT).unwrap();
    }
    assert!(lease_retained);
    assert!(
        dropped_before_release.is_ok(),
        "callback drop waited for state I/O"
    );
    drop(StorageDirectory::open(&directory.0).unwrap());
    reject.assert_calls(1);
}

#[test]
fn capture_screen_and_getters_complete_while_state_io_is_gated() {
    use crate::stateful::CaptureOutcome;
    let directory = Directory::new();
    let (state, gate) = gated_writer(&directory);
    gate.entered.recv_timeout(WAIT).unwrap();
    let client = Arc::new(client_with_writer(&config(), state));
    let capture_client = client.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let caller = thread::spawn(move || {
        done_tx
            .send((
                capture_client.capture("nonblocking"),
                capture_client.screen("screen", Default::default()),
                capture_client.get_distinct_id(),
                capture_client.get_anonymous_id(),
            ))
            .unwrap();
    });
    let result = done_rx.recv_timeout(WAIT);
    gate.release.send(()).unwrap();
    caller.join().unwrap();
    client.shutdown_blocking(None);
    gate.dropped.recv_timeout(WAIT).unwrap();
    let (capture, screen, distinct, anonymous) =
        result.expect("capture or getter waited for state I/O");
    assert!(matches!(capture, CaptureOutcome::Accepted { .. }));
    assert!(matches!(screen, CaptureOutcome::Accepted { .. }));
    assert_eq!(distinct, "anonymous-test-id");
    assert_eq!(anonymous, distinct);
}

#[test]
fn consent_is_immediate_while_io_is_gated_and_old_completion_cannot_revert_it() {
    use crate::stateful::CaptureOutcome;
    let directory = Directory::new();
    let (state, gate) = gated_writer(&directory);
    gate.entered.recv_timeout(WAIT).unwrap();
    let client = Arc::new(client_with_writer(&config(), state));
    let caller_client = client.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let caller = thread::spawn(move || {
        caller_client.opt_out();
        let out = caller_client.is_opt_out();
        let blocked = caller_client.capture("blocked");
        caller_client.opt_in();
        let opted_in = !caller_client.is_opt_out();
        let admitted = caller_client.capture("permitted");
        caller_client.opt_out();
        done_tx.send((out, blocked, opted_in, admitted)).unwrap();
    });
    let result = done_rx.recv_timeout(WAIT);
    // Stop future scheduling while the original, pre-consent write is in flight.
    // This leaves the last durable snapshot older than the current memory choice.
    client.state.as_ref().unwrap().request_close();
    gate.release.send(()).unwrap();
    caller.join().unwrap();
    client.state.as_ref().unwrap().join_blocking();
    let (out, blocked, opted_in, admitted) =
        result.expect("consent/capture waited for filesystem I/O");
    assert!(out && opted_in);
    assert_eq!(blocked, CaptureOutcome::OptedOut);
    assert!(matches!(admitted, CaptureOutcome::Accepted { .. }));
    assert!(
        client.is_opt_out(),
        "old write completion changed live consent"
    );
    client.shutdown_blocking(None);
    drop(client);
    gate.dropped.recv_timeout(WAIT).unwrap();
    let restored = FileStateStore::open(StorageDirectory::open(&directory.0).unwrap())
        .unwrap()
        .snapshot;
    assert_eq!(
        restored.unwrap().opt_out,
        None,
        "pending consent was not durable"
    );
}

#[test]
fn property_and_group_mutations_complete_while_state_io_is_gated() {
    let directory = Directory::new();
    let (state, gate) = gated_writer(&directory);
    gate.entered.recv_timeout(WAIT).unwrap();
    let client = Arc::new(client_with_writer(&config(), state));
    let caller_client = client.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let caller = thread::spawn(move || {
        caller_client.register(std::collections::HashMap::from([(
            "plan".into(),
            serde_json::json!("pro"),
        )]));
        caller_client.group("company", "old", None);
        caller_client.group_identify("company", "metadata-only", std::collections::HashMap::new());
        let before = caller_client.state.as_ref().unwrap().snapshot();
        caller_client.group("company", "new", None);
        caller_client.unregister("plan");
        caller_client.remove_group("company");
        let after = caller_client.state.as_ref().unwrap().snapshot();
        done_tx.send((before, after)).unwrap();
    });
    let result = done_rx.recv_timeout(WAIT);
    client.state.as_ref().unwrap().request_close();
    gate.release.send(()).unwrap();
    caller.join().unwrap();
    client.state.as_ref().unwrap().join_blocking();
    let (before, after) = result.expect("context mutation waited for state I/O");
    assert_eq!(before.registered["plan"], "pro");
    assert_eq!(before.groups["company"], "old");
    assert!(before.person_processing);
    assert!(after.registered.is_empty() && after.groups.is_empty());
    assert!(after.context_revision > before.context_revision);
    let live = client.state.as_ref().unwrap().snapshot();
    // Flag-load completion can advance independently of the gated state write.
    assert!(live.same_persisted_state(&after));
    assert_eq!(live.context_revision, after.context_revision);
    client.shutdown_blocking(None);
    drop(client);
    gate.dropped.recv_timeout(WAIT).unwrap();
    let restored = FileStateStore::open(StorageDirectory::open(&directory.0).unwrap())
        .unwrap()
        .snapshot;
    assert!(restored.unwrap().groups.is_empty());
}

#[test]
fn session_operations_complete_while_state_io_is_gated() {
    let directory = Directory::new();
    let (state, gate) = gated_writer(&directory);
    gate.entered.recv_timeout(WAIT).unwrap();
    state
        .mutate(|snapshot| {
            snapshot.sessions.persistence = super::super::SessionPersistence::Persistent;
        })
        .unwrap();
    let client = Arc::new(client_with_writer(&config(), state));
    let caller_client = client.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let caller = thread::spawn(move || {
        let first = caller_client.session_id().unwrap();
        caller_client.session_activity();
        caller_client.set_app_in_background(true);
        caller_client.set_app_in_background(false);
        caller_client.end_session();
        caller_client.start_session();
        caller_client.reset_session();
        let next = caller_client.peek_session_id().unwrap();
        done_tx.send((first, next)).unwrap();
    });
    let result = done_rx.recv_timeout(WAIT);
    client.state.as_ref().unwrap().request_close();
    gate.release.send(()).unwrap();
    caller.join().unwrap();
    client.shutdown_blocking(None);
    gate.dropped.recv_timeout(WAIT).unwrap();
    let (first, next) = result.expect("session operations waited for state I/O");
    assert_ne!(first, next);
}

#[test]
fn identity_mutations_complete_while_state_io_is_gated() {
    let directory = Directory::new();
    let (state, gate) = gated_writer(&directory);
    gate.entered.recv_timeout(WAIT).unwrap();
    let client = Arc::new(client_with_writer(&config(), state));
    let caller_client = client.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let caller = thread::spawn(move || {
        caller_client.identify("identified");
        let identified = caller_client.is_identified();
        let id = caller_client.get_distinct_id();
        caller_client.alias("alias");
        caller_client.reset();
        caller_client.create_person_profile();
        done_tx
            .send((identified, id, caller_client.is_identified()))
            .unwrap();
    });
    let result = done_rx.recv_timeout(WAIT);
    gate.release.send(()).unwrap();
    caller.join().unwrap();
    client.shutdown_blocking(None);
    gate.dropped.recv_timeout(WAIT).unwrap();
    assert_eq!(
        result.expect("identity mutation waited for filesystem I/O"),
        (true, "identified".into(), false)
    );
}

#[test]
fn closing_from_capture_hook_does_not_wait_for_gated_state_io() {
    use crate::stateful::CaptureOutcome;
    let directory = Directory::new();
    let (state, gate) = gated_writer(&directory);
    gate.entered.recv_timeout(WAIT).unwrap();
    let slot = Arc::new(Mutex::new(std::sync::Weak::<Client>::new()));
    let hook_slot = slot.clone();
    let mut config = config();
    config.before_send(move |event| {
        let client = hook_slot.lock().unwrap().upgrade().unwrap();
        assert_eq!(
            client.shutdown_blocking(None).delivery,
            DeliveryOutcome::Deferred
        );
        Some(event)
    });
    let client = Arc::new(client_with_writer(&config, state));
    *slot.lock().unwrap() = Arc::downgrade(&client);
    let capture_client = client.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let caller = thread::spawn(move || done_tx.send(capture_client.capture("outer")).unwrap());
    let result = done_rx.recv_timeout(WAIT);
    gate.release.send(()).unwrap();
    caller.join().unwrap();
    client.shutdown_blocking(None);
    gate.dropped.recv_timeout(WAIT).unwrap();
    assert_eq!(
        result.expect("hook shutdown waited for state I/O"),
        CaptureOutcome::Closed
    );
}

#[test]
fn expired_sdk_shutdown_and_immediate_drop_do_not_rejoin_blocked_storage() {
    let directory = Directory::new();
    let (state, gate) = gated_writer(&directory);
    gate.entered.recv_timeout(WAIT).unwrap();
    let mut config = config();
    config.shutdown_timeout = Duration::from_millis(20);
    let client = client_with_writer(&config, state);
    let start = Instant::now();
    let report = client.shutdown_blocking(None);
    let waited = start.elapsed();
    assert_eq!(report.state, PersistenceOutcome::TimedOut);
    assert!(
        waited < Duration::from_secs(1),
        "SDK wait exceeded watchdog: {:?}",
        waited
    );
    let selected = client.shutdown.as_ref().unwrap().deadline();
    let repeated = Instant::now();
    assert_eq!(
        client.shutdown_blocking(Some(Duration::from_secs(5))).state,
        PersistenceOutcome::TimedOut
    );
    assert_eq!(client.shutdown.as_ref().unwrap().deadline(), selected);
    assert!(repeated.elapsed() < Duration::from_secs(1));
    let dropping = Instant::now();
    drop(client);
    let dropped = dropping.elapsed();
    assert!(
        dropped < Duration::from_secs(1),
        "Drop rejoined unfinished I/O"
    );
    assert!(StorageDirectory::open(&directory.0).is_err());
    gate.release.send(()).unwrap();
    gate.dropped.recv_timeout(WAIT).unwrap();
    drop(StorageDirectory::open(&directory.0).unwrap());
    eprintln!(
        "blocked-state shutdown_wait_us={} immediate_drop_us={}",
        waited.as_micros(),
        dropped.as_micros()
    );
}

#[test]
fn blocked_event_append_owns_lease_after_deadline_and_drop_without_new_uploads() {
    use crate::capture_event::CaptureEvent;
    use crate::client::event_store::{
        EventStore, FileEventStore, Recovery, StoreKey, StoreLimits, StoreWriteError,
    };
    struct Events {
        inner: Option<FileEventStore>,
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        dropped: mpsc::Sender<()>,
    }
    impl EventStore for Events {
        fn recover(&mut self, n: usize, b: u64) -> io::Result<Recovery> {
            self.inner.as_mut().unwrap().recover(n, b)
        }
        fn head(&mut self, n: usize, b: u64) -> io::Result<Recovery> {
            self.inner.as_mut().unwrap().head(n, b)
        }
        fn end_key(&self) -> Option<StoreKey> {
            self.inner.as_ref().unwrap().end_key()
        }
        fn contains(&self, key: StoreKey) -> io::Result<bool> {
            self.inner.as_ref().unwrap().contains(key)
        }
        fn put(&mut self, event: &CaptureEvent) -> Result<StoreKey, StoreWriteError> {
            self.inner.as_mut().unwrap().put(event)
        }
        fn put_at(
            &mut self,
            event: &CaptureEvent,
            key: StoreKey,
        ) -> Result<StoreKey, StoreWriteError> {
            self.entered.send(()).unwrap();
            self.release.recv_timeout(GATE_WAIT).unwrap();
            self.inner.as_mut().unwrap().put_at(event, key)
        }
        fn remove(&mut self, key: StoreKey) -> io::Result<()> {
            self.inner.as_mut().unwrap().remove(key)
        }
    }
    impl Drop for Events {
        fn drop(&mut self) {
            drop(self.inner.take());
            let _ = self.dropped.send(());
        }
    }
    let directory = Directory::new();
    let server = MockServer::start();
    let upload = server.mock(|when, then| {
        when.method(POST);
        then.status(200);
    });
    let mut config = config();
    config.host = server.base_url();
    config.shutdown_timeout = Duration::from_millis(20);
    let state = StateWriter::new(
        Snapshot::anonymous("anonymous".into()),
        Backend::Memory,
        WriterOptions::default(),
        vec![],
        Arc::new(SystemClock),
    )
    .unwrap();
    let (entered, wait) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let (dropped, done) = mpsc::channel();
    let store = FileEventStore::open(
        StorageDirectory::open(&directory.0).unwrap(),
        StoreLimits {
            max_events: 10,
            max_bytes: 64 * 1024,
            max_record_bytes: 4096,
        },
    )
    .unwrap();
    let client = client_with_store(
        &config,
        state,
        Some(Box::new(Events {
            inner: Some(store),
            entered,
            release: gate,
            dropped,
        })),
    );
    assert!(matches!(
        client.capture("blocked-write"),
        super::super::CaptureOutcome::Accepted { .. }
    ));
    wait.recv_timeout(WAIT).unwrap();
    let start = Instant::now();
    assert_eq!(
        client.shutdown_blocking(None).delivery,
        DeliveryOutcome::TimedOut
    );
    drop(client);
    let elapsed = start.elapsed();
    let still_owned = StorageDirectory::open(&directory.0).is_err();
    release.send(()).unwrap();
    done.recv_timeout(WAIT).unwrap();
    assert!(still_owned);
    assert!(elapsed < Duration::from_secs(1));
    drop(StorageDirectory::open(&directory.0).unwrap());
    upload.assert_calls(0);
    assert_eq!(
        std::fs::read_dir(directory.0.join("events"))
            .unwrap()
            .count(),
        1
    );
    eprintln!("blocked-event shutdown_and_drop_us={}", elapsed.as_micros());
}

#[test]
fn request_only_work_does_not_keep_event_storage_leased_after_delivery_stops() {
    use crate::client::event_store::{FileEventStore, StoreLimits};
    use std::io::{Read, Write};
    let directory = Directory::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let host = format!("http://{}", listener.local_addr().unwrap());
    let (entered, waiting) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 4096];
        assert!(stream.read(&mut request).unwrap() > 0);
        entered.send(()).unwrap();
        gate.recv_timeout(GATE_WAIT).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
    });
    let mut config = config();
    config.host = host;
    config.shutdown_timeout = Duration::from_millis(20);
    let state = StateWriter::new(
        Snapshot::anonymous("anonymous".into()),
        Backend::Memory,
        WriterOptions::default(),
        vec![],
        Arc::new(SystemClock),
    )
    .unwrap();
    let store = FileEventStore::open(
        StorageDirectory::open(&directory.0).unwrap(),
        StoreLimits {
            max_events: 10,
            max_bytes: 64 * 1024,
            max_record_bytes: 4096,
        },
    )
    .unwrap();
    let client = client_with_store(&config, state, Some(Box::new(store)));
    client.load_remote_config(false);
    waiting.recv_timeout(WAIT).unwrap();
    let start = Instant::now();
    client.shutdown_blocking(None);
    let elapsed = start.elapsed();
    let end = Instant::now() + WAIT;
    while !client.transport.as_ref().unwrap().delivery_stopped() && Instant::now() < end {
        thread::yield_now();
    }
    let leased = StorageDirectory::open(&directory.0);
    let dropping = Instant::now();
    drop(client);
    let dropped = dropping.elapsed();
    release.send(()).unwrap();
    server.join().unwrap();
    assert!(
        leased.is_ok(),
        "request-only completion retained event storage"
    );
    assert!(elapsed < Duration::from_secs(1));
    assert!(dropped < Duration::from_secs(1));
    eprintln!(
        "blocked-request shutdown_wait_us={} immediate_drop_us={}",
        elapsed.as_micros(),
        dropped.as_micros()
    );
}

use super::*;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread::{self, ThreadId};
use std::time::Duration;

use crate::client::event_store::StorageDirectory;
use crate::client::stateful::storage::{FileStateStore, StateStore};
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
    fn save(&mut self, snapshot: &Snapshot) -> io::Result<()> {
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
    let inner = FileStateStore::open(StorageDirectory::open(&directory.0).unwrap(), 4096)
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

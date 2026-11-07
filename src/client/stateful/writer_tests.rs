use super::*;
use crate::client::event_store::StorageDirectory;
use crate::client::stateful::storage::FileStateStore;
use crate::client::transport::SystemClock;
use crate::client::PostHogError;
use chrono::{DateTime, Utc};
use std::sync::{mpsc, Weak};

struct ManualClock(Mutex<Instant>);

impl ManualClock {
    fn new() -> Self {
        Self(Mutex::new(Instant::now()))
    }
    fn advance(&self, elapsed: Duration) {
        *self.0.lock().unwrap() += elapsed;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

struct WriteCall {
    snapshot: Snapshot,
    thread: ThreadId,
    reply: mpsc::Sender<io::Result<()>>,
}

impl WriteCall {
    fn finish(self, result: io::Result<()>) {
        self.reply.send(result).unwrap();
    }
}

struct ControlledStore(mpsc::Sender<WriteCall>);

impl StateStore for ControlledStore {
    fn save(&mut self, snapshot: &Snapshot) -> io::Result<()> {
        let (reply, completion) = mpsc::channel();
        self.0
            .send(WriteCall {
                snapshot: snapshot.clone(),
                thread: thread::current().id(),
                reply,
            })
            .map_err(|_| io::Error::other("test controller disconnected"))?;
        completion
            .recv()
            .unwrap_or_else(|_| Err(io::Error::other("test write interrupted")))
    }
}

fn options() -> WriterOptions {
    WriterOptions {
        max_bytes: 4096,
        shutdown_timeout: Duration::ZERO,
        ..WriterOptions::default()
    }
}

fn controlled(
    hooks: Vec<OnErrorHook>,
) -> (
    Arc<StateWriter>,
    mpsc::Receiver<WriteCall>,
    Arc<ManualClock>,
) {
    controlled_with_options(hooks, options())
}

fn controlled_with_options(
    hooks: Vec<OnErrorHook>,
    settings: WriterOptions,
) -> (
    Arc<StateWriter>,
    mpsc::Receiver<WriteCall>,
    Arc<ManualClock>,
) {
    let (tx, rx) = mpsc::channel();
    let clock = Arc::new(ManualClock::new());
    let writer = StateWriter::new(
        Snapshot::anonymous("initial".into()),
        Backend::Persistent {
            store: Box::new(ControlledStore(tx)),
            restored: true,
        },
        settings,
        hooks,
        clock.clone(),
    )
    .unwrap();
    (Arc::new(writer), rx, clock)
}

fn call(rx: &mpsc::Receiver<WriteCall>) -> WriteCall {
    rx.recv_timeout(Duration::from_secs(5))
        .expect("worker should reach the controlled store")
}

/// Wake a due write without waiting for the backend, whose completion this
/// test controls. Locking state prevents losing the wake before the worker waits.
fn wake_writer(writer: &StateWriter) {
    let _state = writer.shared.lock();
    writer.shared.changed.notify_all();
}

fn replace(writer: &StateWriter, id: &str) -> u64 {
    writer
        .update(|state| *state = Snapshot::anonymous(id.into()))
        .unwrap()
}

#[test]
fn blocked_io_does_not_delay_reads_or_updates_and_only_latest_pending_state_is_written() {
    let (writer, calls, _clock) = controlled(vec![]);
    replace(&writer, "first");
    writer.checkpoint();
    let first = call(&calls);
    assert_ne!(first.thread, thread::current().id());
    assert_eq!(first.snapshot.anonymous_id, "first");
    let producer = writer.clone();
    let (tx, rx) = mpsc::channel();
    let producer_thread = thread::spawn(move || {
        for index in 0..1000 {
            replace(&producer, &format!("next-{index}"));
        }
        tx.send((
            producer.snapshot().anonymous_id.clone(),
            producer.checkpoint(),
        ))
        .unwrap();
    });
    let immediate = rx.recv_timeout(Duration::from_secs(5));
    // Release the store even if the assertion fails, so a regression cannot
    // strand the test's writer destructor behind the controlled I/O.
    first.finish(Ok(()));
    let (id, revision) = immediate.expect("memory operations must not await the store");
    producer_thread.join().unwrap();
    assert_eq!(id, "next-999");
    assert_eq!(revision, 1002);
    let latest = call(&calls);
    assert_eq!(latest.snapshot.anonymous_id, "next-999");
    assert_eq!(latest.thread, writer.worker_id.unwrap());
    latest.finish(Ok(()));
    assert_eq!(writer.wait_blocking(revision), DrainOutcome::Persisted);
    writer.tick();
    assert!(calls.try_recv().is_err());
}

#[test]
fn waking_an_in_flight_write_does_not_wait_for_test_owned_completion() {
    let (writer, calls, _) = controlled(vec![]);
    replace(&writer, "pending");
    writer.checkpoint();
    let pending = call(&calls);
    let producer = writer.clone();
    let (tx, rx) = mpsc::channel();
    let wake = thread::spawn(move || {
        wake_writer(&producer);
        tx.send(()).unwrap();
    });
    let result = rx.recv_timeout(Duration::from_secs(5));
    pending.finish(Ok(()));
    wake.join().unwrap();
    assert!(
        result.is_ok(),
        "scheduler wake must not wait for test-owned I/O"
    );
}

#[test]
fn continuous_updates_do_not_postpone_the_first_coalescing_deadline() {
    let (writer, calls, clock) = controlled(vec![]);
    let deadline = clock.now() + Duration::from_millis(250);
    replace(&writer, "first");
    for index in 0..4 {
        clock.advance(Duration::from_millis(50));
        replace(&writer, &format!("latest-{index}"));
        writer.tick();
        let state = writer.shared.lock();
        assert_eq!(state.due, Some(deadline));
        assert!(state.in_flight.is_none());
    }
    assert!(calls.try_recv().is_err());
    clock.advance(Duration::from_millis(50));
    wake_writer(&writer);
    let latest = call(&calls);
    assert_eq!(latest.snapshot.anonymous_id, "latest-3");
    latest.finish(Ok(()));
    let revision = writer.checkpoint();
    assert_eq!(writer.wait_blocking(revision), DrainOutcome::Persisted);
}

#[test]
fn captured_revision_barrier_does_not_chase_later_updates() {
    let (writer, calls, _clock) = controlled(vec![]);
    replace(&writer, "before-barrier");
    let before = writer.checkpoint();
    let pending = call(&calls);
    let after = replace(&writer, "after-barrier");
    assert!(after > before);
    pending.finish(Ok(()));
    assert_eq!(writer.wait_blocking(before), DrainOutcome::Persisted);
    writer.tick();
    assert_eq!(writer.snapshot().anonymous_id, "after-barrier");
    assert_eq!(writer.shared.lock().persisted, before);
    assert!(calls.try_recv().is_err());
    writer.request_close();
    assert_eq!(writer.join_blocking(), DrainOutcome::TimedOut);
}

#[test]
fn failed_writes_keep_dirty_memory_and_retry_latest_state_without_bypassing_cooldown() {
    let (errors_tx, errors_rx) = mpsc::channel();
    let (writer, calls, clock) = controlled(vec![OnErrorHook::new(move |failure| {
        if let PostHogError::Storage(error) = failure {
            errors_tx
                .send((error.operation(), thread::current().id()))
                .unwrap();
        }
    })]);
    replace(&writer, "first");
    let revision = writer.checkpoint();
    call(&calls).finish(Err(io::Error::other("test write failure")));
    assert_eq!(writer.wait_blocking(revision), DrainOutcome::WriteFailed);
    assert_eq!(
        errors_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        (StorageOperation::Write, writer.worker_id.unwrap())
    );
    assert_eq!(writer.snapshot().anonymous_id, "first");
    let due = clock.now() + Duration::from_millis(100);
    let latest = replace(&writer, "latest");
    assert_eq!(writer.checkpoint(), latest);
    clock.advance(Duration::from_millis(99));
    writer.tick();
    {
        let state = writer.shared.lock();
        assert_eq!(state.persisted, 1);
        assert_eq!(state.due, Some(due));
        assert!(state.in_flight.is_none());
    }
    clock.advance(Duration::from_millis(1));
    wake_writer(&writer);
    let retry = call(&calls);
    assert_eq!(retry.snapshot.anonymous_id, "latest");
    retry.finish(Err(io::Error::other("second test failure")));
    assert_eq!(writer.wait_blocking(latest), DrainOutcome::WriteFailed);
    errors_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        writer.shared.lock().due,
        Some(clock.now() + Duration::from_millis(200))
    );
    clock.advance(Duration::from_millis(200));
    wake_writer(&writer);
    call(&calls).finish(Ok(()));
    writer.tick();
    assert_eq!(writer.wait_blocking(latest), DrainOutcome::Persisted);
    let state = writer.shared.lock();
    assert_eq!(state.failures, 0);
    assert!(state.failure.is_none());
    assert!(state.due.is_none());
}

#[test]
fn shutdown_respects_retry_cooldown_and_a_fixed_deadline() {
    let (writer, calls, clock) = controlled_with_options(
        vec![],
        WriterOptions {
            shutdown_timeout: Duration::from_millis(250),
            ..options()
        },
    );
    let revision = replace(&writer, "pending");
    writer.checkpoint();
    call(&calls).finish(Err(io::Error::other("first failure")));
    assert_eq!(writer.wait_blocking(revision), DrainOutcome::WriteFailed);
    let deadline = clock.now() + Duration::from_millis(250);
    writer.request_close();
    clock.advance(Duration::from_millis(99));
    writer.request_close();
    writer.tick();
    {
        let state = writer.shared.lock();
        assert_eq!(state.closing, Some(deadline));
        assert!(state.in_flight.is_none());
    }
    clock.advance(Duration::from_millis(1));
    wake_writer(&writer);
    call(&calls).finish(Err(io::Error::other("second failure")));
    writer.tick();
    assert_eq!(
        writer.shared.lock().due,
        Some(clock.now() + Duration::from_millis(200))
    );
    clock.advance(Duration::from_millis(150));
    writer.tick();
    assert_eq!(writer.join_blocking(), DrainOutcome::WriteFailed);
    assert!(calls.recv_timeout(Duration::from_secs(5)).is_err());
}

#[test]
fn shutdown_does_not_start_a_newer_write_after_its_deadline() {
    let (writer, calls, clock) = controlled_with_options(
        vec![],
        WriterOptions {
            shutdown_timeout: Duration::from_millis(100),
            ..options()
        },
    );
    replace(&writer, "in-flight");
    writer.checkpoint();
    let pending = call(&calls);
    replace(&writer, "newer");
    writer.request_close();
    clock.advance(Duration::from_millis(101));
    pending.finish(Ok(()));
    assert_eq!(writer.join_blocking(), DrainOutcome::TimedOut);
    assert_eq!(writer.snapshot().anonymous_id, "newer");
    assert!(calls.recv_timeout(Duration::from_secs(5)).is_err());
}

#[test]
fn no_op_invalid_and_oversized_mutations_do_not_change_memory_or_revision() {
    let writer = StateWriter::new(
        Snapshot::anonymous("initial".into()),
        Backend::Memory,
        options(),
        vec![],
        Arc::new(SystemClock),
    )
    .unwrap();
    assert_eq!(writer.update(|_| {}), Ok(1));
    assert_eq!(
        writer.update(|state| state.anonymous_id.clear()),
        Err(UpdateError::Invalid)
    );
    assert_eq!(
        writer.update(|state| state.distinct_id = "x".repeat(4096)),
        Err(UpdateError::Full)
    );
    assert_eq!(*writer.snapshot(), Snapshot::anonymous("initial".into()));
    assert_eq!(writer.checkpoint(), 1);
    assert_eq!(writer.wait_blocking(1), DrainOutcome::MemoryOnly);
    writer.request_close();
    assert!(writer.is_closed());
    assert_eq!(
        writer.update(|_| panic!("closed mutation must not run")),
        Err(UpdateError::Closed)
    );
    assert_eq!(writer.join_blocking(), DrainOutcome::MemoryOnly);
}

#[test]
fn concurrent_mutations_are_synchronously_visible_without_lost_updates() {
    let writer = Arc::new(
        StateWriter::new(
            Snapshot::anonymous("0".into()),
            Backend::Memory,
            options(),
            vec![],
            Arc::new(SystemClock),
        )
        .unwrap(),
    );
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let writer = writer.clone();
            thread::spawn(move || {
                for _ in 0..100 {
                    writer
                        .update(|state| {
                            let value: u64 = state.distinct_id.parse().unwrap();
                            state.distinct_id = (value + 1).to_string();
                        })
                        .unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(writer.snapshot().distinct_id, "800");
    assert_eq!(writer.checkpoint(), 801);
    assert!(writer.worker_id.is_none());
}

#[test]
fn unavailable_backend_is_distinct_from_intentional_memory_mode() {
    let writer = StateWriter::new(
        Snapshot::anonymous("initial".into()),
        Backend::Unavailable,
        options(),
        vec![],
        Arc::new(SystemClock),
    )
    .unwrap();
    let revision = replace(&writer, "still-usable");
    assert_eq!(writer.wait_blocking(revision), DrainOutcome::Unavailable);
    assert_eq!(writer.snapshot().anonymous_id, "still-usable");
    writer.request_close();
    assert_eq!(writer.join_blocking(), DrainOutcome::Unavailable);
}

#[test]
fn explicit_shutdown_deadline_is_shared_and_cannot_be_extended_by_later_close() {
    let (writer, calls, clock) = controlled(vec![]);
    replace(&writer, "dirty");
    writer.checkpoint();
    let pending = call(&calls);
    let deadline = clock.now() + Duration::from_secs(1);
    writer.request_close_before(deadline);
    writer.request_close_before(deadline + Duration::from_secs(10));
    writer.request_close();
    assert_eq!(writer.shared.lock().closing, Some(deadline));
    clock.advance(Duration::from_secs(1));
    pending.finish(Err(io::Error::other("test failure")));
    assert_eq!(writer.join_blocking(), DrainOutcome::WriteFailed);
}

#[test]
fn shutdown_from_error_hook_is_deferred_without_self_join_or_state_lock_deadlock() {
    let reference = Arc::new(Mutex::new(Weak::<StateWriter>::new()));
    let weak = reference.clone();
    let (tx, rx) = mpsc::channel();
    let (writer, calls, _clock) = controlled(vec![OnErrorHook::new(move |_| {
        let writer = weak.lock().unwrap().upgrade().unwrap();
        assert!(writer.on_worker_thread());
        assert_eq!(writer.snapshot().anonymous_id, "latest");
        writer.request_close();
        tx.send(writer.join_blocking()).unwrap();
    })]);
    *reference.lock().unwrap() = Arc::downgrade(&writer);
    replace(&writer, "latest");
    writer.checkpoint();
    call(&calls).finish(Err(io::Error::other("test write failure")));
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        DrainOutcome::Deferred
    );
    assert_eq!(writer.join_blocking(), DrainOutcome::WriteFailed);
    writer.request_close();
    assert_eq!(writer.join_blocking(), DrainOutcome::WriteFailed);
    assert_eq!(writer.update(|_| {}), Err(UpdateError::Closed));
}

#[test]
fn shutdown_does_not_interrupt_an_inflight_write_and_waits_for_backend_release() {
    let (writer, calls, _clock) = controlled(vec![]);
    let revision = replace(&writer, "in-flight");
    writer.checkpoint();
    let pending = call(&calls);
    writer.request_close();
    assert!(!writer.shared.lock().stopped);
    assert_eq!(writer.update(|_| {}), Err(UpdateError::Closed));
    pending.finish(Ok(()));
    assert_eq!(writer.join_blocking(), DrainOutcome::Persisted);
    assert_eq!(writer.wait_blocking(revision), DrainOutcome::Persisted);
    assert!(calls.recv_timeout(Duration::from_secs(5)).is_err());
}

#[test]
fn worker_panic_degrades_to_memory_and_wakes_waiters() {
    struct PanickingStore;
    impl StateStore for PanickingStore {
        fn save(&mut self, _: &Snapshot) -> io::Result<()> {
            panic!("test backend failure")
        }
    }
    let (tx, rx) = mpsc::channel();
    let writer = StateWriter::new(
        Snapshot::anonymous("initial".into()),
        Backend::Persistent {
            store: Box::new(PanickingStore),
            restored: false,
        },
        options(),
        vec![OnErrorHook::new(move |failure| {
            if let PostHogError::Storage(error) = failure {
                tx.send(error.operation()).unwrap();
            }
        })],
        Arc::new(SystemClock),
    )
    .unwrap();
    assert_eq!(
        writer.wait_blocking(writer.checkpoint()),
        DrainOutcome::Unavailable
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        StorageOperation::Write
    );
    assert_eq!(writer.snapshot().anonymous_id, "initial");
    let revision = replace(&writer, "after-failure");
    assert_eq!(writer.wait_blocking(revision), DrainOutcome::Unavailable);
    writer.request_close();
    assert_eq!(writer.join_blocking(), DrainOutcome::Unavailable);
}

#[test]
fn destructor_panic_cannot_leave_shutdown_waiters_asleep() {
    struct PanicOnDrop;
    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("test observer destructor")
        }
    }
    let guard = PanicOnDrop;
    let (writer, _calls, _clock) = controlled(vec![OnErrorHook::new(move |_| {
        let _ = &guard;
    })]);
    writer.request_close();
    assert_eq!(writer.join_blocking(), DrainOutcome::Unavailable);
}

#[test]
fn new_and_updated_identity_survive_real_file_shutdown_and_restart() {
    let path = std::env::temp_dir().join(format!("posthog-state-writer-{}", uuid::Uuid::now_v7()));
    let (store, initial) =
        FileStateStore::open(StorageDirectory::open(&path).unwrap(), 4096).unwrap();
    assert!(initial.is_none());
    let writer = StateWriter::new(
        Snapshot::anonymous("initial".into()),
        Backend::Persistent {
            store: Box::new(store),
            restored: false,
        },
        WriterOptions {
            shutdown_timeout: Duration::from_secs(5),
            ..options()
        },
        vec![],
        Arc::new(SystemClock),
    )
    .unwrap();
    assert_eq!(
        writer.wait_blocking(writer.checkpoint()),
        DrainOutcome::Persisted
    );
    replace(&writer, "latest");
    writer.request_close();
    assert_eq!(writer.join_blocking(), DrainOutcome::Persisted);
    drop(writer);
    let loaded = FileStateStore::open(StorageDirectory::open(&path).unwrap(), 4096)
        .unwrap()
        .1;
    assert_eq!(loaded, Some(Snapshot::anonymous("latest".into())));
    std::fs::remove_dir_all(path).unwrap();
}

#[cfg(feature = "async-client")]
#[test]
fn async_drains_and_shutdown_need_no_owned_runtime_and_match_blocking_results() {
    use std::future::Future;
    use std::task::{Context, Poll};
    let (writer, calls, _clock) = controlled(vec![]);
    replace(&writer, "latest");
    let revision = writer.checkpoint();
    let pending = call(&calls);
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut wait = Box::pin(writer.wait_async(revision));
    assert!(matches!(wait.as_mut().poll(&mut cx), Poll::Pending));
    drop(wait); // Cancelled waiters do not cancel a state write or lose a later wake.
    pending.finish(Ok(()));
    assert_eq!(
        futures::executor::block_on(writer.wait_async(revision)),
        DrainOutcome::Persisted
    );
    assert_eq!(writer.wait_blocking(revision), DrainOutcome::Persisted);
    writer.request_close();
    assert_eq!(
        futures::executor::block_on(writer.join_async()),
        DrainOutcome::Persisted
    );
    assert_eq!(writer.join_blocking(), DrainOutcome::Persisted);
    fn assert_send(_: impl Send) {}
    assert_send(writer.wait_async(revision));
    assert_send(writer.join_async());
}

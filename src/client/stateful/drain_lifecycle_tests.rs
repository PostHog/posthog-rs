use super::*;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::FutureExt;

type InlineFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
struct InlineTask(Mutex<Option<InlineFuture>>);

impl InlineTask {
    fn new(future: impl Future<Output = ()> + Send + 'static) -> Arc<Self> {
        Arc::new(Self(Mutex::new(Some(Box::pin(future)))))
    }

    fn poll(task: &Arc<Self>) -> Poll<()> {
        let waker = futures::task::waker(task.clone());
        let mut context = Context::from_waker(&waker);
        let mut slot = task.0.lock().unwrap();
        let Some(future) = slot.as_mut() else {
            return Poll::Ready(());
        };
        let result = future.as_mut().poll(&mut context);
        if result.is_ready() {
            slot.take();
        }
        result
    }
}

impl futures::task::ArcWake for InlineTask {
    fn wake_by_ref(task: &Arc<Self>) {
        let _ = Self::poll(task);
    }
}

#[test]
fn async_writer_join_resumed_inline_on_its_worker_defers() {
    let directory = Directory::new();
    let (state, gate) = gated_writer(&directory);
    gate.entered.recv_timeout(WAIT).unwrap();
    let state = Arc::new(state);
    state.request_close_before(Instant::now() + Duration::from_secs(30));
    let waiting = state.clone();
    let (tx, rx) = mpsc::channel();
    let task = InlineTask::new(async move {
        let result = std::panic::AssertUnwindSafe(waiting.join_async())
            .catch_unwind()
            .await;
        tx.send((thread::current().id(), result)).unwrap();
    });
    assert!(InlineTask::poll(&task).is_pending());
    gate.release.send(()).unwrap();
    let (resumed_on, result) = rx.recv_timeout(WAIT).unwrap();
    let worker = gate.dropped.recv_timeout(WAIT).unwrap();
    assert_eq!(resumed_on, worker);
    assert_eq!(
        result.unwrap(),
        super::super::super::writer::DrainOutcome::Deferred
    );
    assert_eq!(
        state.join_blocking(),
        super::super::super::writer::DrainOutcome::Persisted
    );
}

#[test]
fn async_shutdown_resumed_inline_on_transport_worker_defers() {
    let server = MockServer::start();
    let rejected = server.mock(|when, then| {
        when.method(POST);
        then.status(400);
    });
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let mut config = config();
    config.host = server.base_url();
    config.flush_at = 1;
    config.preload_feature_flags = false;
    config.on_error(move |failure| {
        if !matches!(failure, crate::PostHogError::Capture(_)) {
            return;
        }
        entered_tx.send(thread::current().id()).unwrap();
        release_rx.lock().unwrap().recv_timeout(GATE_WAIT).unwrap();
    });
    let client = Arc::new(Client::init_blocking(config));
    client.capture("rejected");
    let worker = entered_rx.recv_timeout(WAIT).unwrap();
    let waiting = client.clone();
    let (tx, rx) = mpsc::channel();
    let task = InlineTask::new(async move {
        let result = std::panic::AssertUnwindSafe(waiting.shutdown(Some(Duration::from_secs(30))))
            .catch_unwind()
            .await;
        tx.send((thread::current().id(), result)).unwrap();
    });
    assert!(InlineTask::poll(&task).is_pending());
    release_tx.send(()).unwrap();
    let (resumed_on, result) = rx.recv_timeout(WAIT).unwrap();
    assert_eq!(resumed_on, worker);
    assert_eq!(result.unwrap().delivery, DeliveryOutcome::Deferred);
    assert_eq!(
        client.shutdown_blocking(None).delivery,
        DeliveryOutcome::Stopped
    );
    rejected.assert_calls(1);
}

#[test]
fn async_flush_captures_revision_and_cancellation_keeps_client_open() {
    let directory = Directory::new();
    let (state, gate) = gated_writer(&directory);
    gate.entered.recv_timeout(WAIT).unwrap();
    let client = client_with_writer(&config(), state);
    let mut context = Context::from_waker(futures::task::noop_waker_ref());
    let mut cancelled = Box::pin(client.flush());
    assert!(matches!(
        cancelled.as_mut().poll(&mut context),
        Poll::Pending
    ));
    drop(cancelled);
    assert!(!client.closed.load(Ordering::Acquire));
    let mut flush = Box::pin(client.flush());
    assert!(matches!(flush.as_mut().poll(&mut context), Poll::Pending));
    let state = client.state.as_ref().unwrap();
    state
        .update(|snapshot| snapshot.distinct_id = "newer".into())
        .unwrap();
    state.checkpoint();
    gate.release.send(()).unwrap();
    // The next revision is now blocked. This flush only requested revision 1.
    gate.entered.recv_timeout(WAIT).unwrap();
    let report = futures::executor::block_on(flush);
    gate.release.send(()).unwrap();
    assert_eq!(report.state, PersistenceOutcome::Persisted);
    assert_eq!(report.delivery, DeliveryOutcome::Attempted);
    client.shutdown_blocking(None);
    gate.dropped.recv_timeout(WAIT).unwrap();
}

#[test]
fn async_drains_from_state_writer_callback_defer_without_cross_worker_waits() {
    struct FailingStore;
    impl StateStore for FailingStore {
        fn save(&mut self, _: &Snapshot) -> io::Result<SaveOutcome> {
            Err(io::Error::other("test state write failure"))
        }
    }
    let slot = Arc::new(Mutex::new(std::sync::Weak::<Client>::new()));
    let callback_slot = slot.clone();
    let (tx, rx) = mpsc::channel();
    let mut config = config();
    config.on_error(move |_| {
        let client = callback_slot.lock().unwrap().upgrade().unwrap();
        let flush = futures::executor::block_on(client.flush());
        let shutdown = futures::executor::block_on(client.shutdown(None));
        tx.send((flush, shutdown)).unwrap();
    });
    let state = StateWriter::new(
        Snapshot::anonymous("anonymous".into()),
        Backend::Persistent {
            store: Box::new(FailingStore),
            restored: true,
        },
        WriterOptions::default(),
        config.on_error.clone(),
        Arc::new(SystemClock),
    )
    .unwrap();
    let client = Arc::new(client_with_writer(&config, state));
    *slot.lock().unwrap() = Arc::downgrade(&client);
    client
        .state
        .as_ref()
        .unwrap()
        .update(|s| s.distinct_id = "newer".into())
        .unwrap();
    client.state.as_ref().unwrap().checkpoint();
    let (flush, shutdown) = rx.recv_timeout(WAIT).unwrap();
    assert_eq!(flush.state, PersistenceOutcome::Deferred);
    assert_eq!(shutdown.delivery, DeliveryOutcome::Deferred);
    let report = futures::executor::block_on(client.shutdown(None));
    assert_eq!(report.state, PersistenceOutcome::WriteFailed);
    assert_eq!(report.delivery, DeliveryOutcome::Stopped);
}

#[test]
fn cancelled_shutdown_wait_keeps_both_workers_closing_and_allows_another_waiter() {
    let directory = Directory::new();
    let (state, gate) = gated_writer(&directory);
    gate.entered.recv_timeout(WAIT).unwrap();
    let client = Arc::new(client_with_writer(&config(), state));
    let mut context = Context::from_waker(futures::task::noop_waker_ref());
    let mut cancelled = Box::pin(client.shutdown(Some(Duration::from_secs(30))));
    assert!(matches!(
        cancelled.as_mut().poll(&mut context),
        Poll::Pending
    ));
    drop(cancelled);
    assert!(client.transport.as_ref().unwrap().is_closed());
    assert!(client.state.as_ref().unwrap().is_closed());
    assert_eq!(
        client.capture("closed"),
        crate::stateful::CaptureOutcome::Closed
    );
    let mut second = Box::pin(client.shutdown(None));
    assert!(matches!(second.as_mut().poll(&mut context), Poll::Pending));
    let blocking_client = client.clone();
    let blocking = thread::spawn(move || blocking_client.shutdown_blocking(None));
    gate.release.send(()).unwrap();
    let async_report = futures::executor::block_on(second);
    let blocking_report = blocking.join().unwrap();
    assert_eq!(async_report, blocking_report);
    assert_eq!(async_report.state, PersistenceOutcome::Persisted);
    assert_eq!(async_report.delivery, DeliveryOutcome::Stopped);
    gate.dropped.recv_timeout(WAIT).unwrap();
    drop(StorageDirectory::open(&directory.0).unwrap());
}

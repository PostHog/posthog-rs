use super::*;
use crate::embedding::{EmbeddingOptions, EventStore};
use std::io;
use std::sync::Condvar;

#[derive(Default)]
struct Store {
    events: Mutex<Vec<Event>>,
    block_put: Mutex<bool>,
    entered: Condvar,
    puts: AtomicUsize,
    release: Condvar,
    fail_purge: AtomicBool,
}

impl EventStore for Store {
    fn load(&self) -> io::Result<Vec<Event>> {
        Ok(self.events.lock().unwrap().clone())
    }
    fn put(&self, event: &Event) -> io::Result<()> {
        let mut block = self.block_put.lock().unwrap();
        self.puts.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_all();
        while *block {
            block = self.release.wait(block).unwrap();
        }
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
    fn remove(&self, ids: &[uuid::Uuid]) -> io::Result<()> {
        self.events
            .lock()
            .unwrap()
            .retain(|event| !ids.contains(&event.uuid()));
        Ok(())
    }
    fn purge(&self) -> io::Result<()> {
        if self.fail_purge.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "test"));
        }
        self.events.lock().unwrap().clear();
        Ok(())
    }
}

impl Store {
    fn wait_for_put(&self) {
        let mut block = self.block_put.lock().unwrap();
        while self.puts.load(Ordering::SeqCst) == 0 {
            let (next, timeout) = self
                .entered
                .wait_timeout(block, Duration::from_secs(5))
                .unwrap();
            assert!(!timeout.timed_out(), "worker did not start persistence");
            block = next;
        }
    }
    fn unblock(&self) {
        *self.block_put.lock().unwrap() = false;
        self.release.notify_all();
    }
}

fn options(store: Arc<Store>) -> ClientOptions {
    crate::ClientOptionsBuilder::default()
        .api_key("phc_test".into())
        .host("http://127.0.0.1:1")
        .flush_at(100usize)
        .flush_interval_ms(60_000u64)
        .embedding(EmbeddingOptions {
            store,
            sdk_name: "posthog-unreal".into(),
            sdk_version: "0.0.1".into(),
            enabled: true,
            replay: None,
        })
        .shutdown_timeout_ms(100u64)
        .build()
        .unwrap()
}

#[test]
fn stale_queued_resume_cannot_override_later_opt_out() {
    let store = Arc::new(Store::default());
    *store.block_put.lock().unwrap() = true;
    let handle = TransportHandle::spawn(options(store.clone()));
    handle.enqueue(Event::new("one", "user"));
    store.wait_for_put();
    let (tx, rx) = mpsc::channel();
    let generation = handle
        .gate
        .as_ref()
        .unwrap()
        .generation
        .load(Ordering::Acquire);
    handle.send_control(Control::Resume {
        generation,
        completion: tx,
    });
    handle.pause_and_purge();
    store.unblock();
    assert!(!rx.recv_timeout(Duration::from_secs(5)).unwrap());
    handle.checkpoint();
    assert_eq!(
        handle.enqueue_checked(Event::new("blocked", "user")),
        EnqueueResult::Disabled
    );
    assert!(store.events.lock().unwrap().is_empty());
    handle.shutdown_blocking();
}

#[test]
fn expired_shutdown_does_not_start_remaining_disk_writes() {
    let store = Arc::new(Store::default());
    *store.block_put.lock().unwrap() = true;
    let handle = TransportHandle::spawn(options(store.clone()));
    handle.enqueue(Event::new("first", "user"));
    store.wait_for_put();
    for _ in 0..10 {
        handle.enqueue(Event::new("later", "user"));
    }
    handle.request_close();
    // Drive the shared deadline directly; no wall-clock sleep is necessary.
    *handle.gate.as_ref().unwrap().deadline.lock().unwrap() = Some(Instant::now());
    store.unblock();
    handle.join();
    assert_eq!(store.puts.load(Ordering::SeqCst), 1);
    assert_eq!(store.events.lock().unwrap().len(), 1);
}

#[test]
fn failed_purge_keeps_admission_closed_until_successful_resume() {
    let store = Arc::new(Store::default());
    let handle = TransportHandle::spawn(options(store.clone()));
    handle.enqueue(Event::new("event", "user"));
    handle.checkpoint();
    store.fail_purge.store(true, Ordering::SeqCst);
    handle.pause_and_purge();
    assert!(!handle.resume(handle.generation()));
    assert_eq!(
        handle.enqueue_checked(Event::new("blocked", "user")),
        EnqueueResult::Disabled
    );
    assert!(handle.checkpoint().persistence_errors > 0);
    store.fail_purge.store(false, Ordering::SeqCst);
    assert!(handle.resume(handle.generation()));
    assert!(store.events.lock().unwrap().is_empty());
    handle.shutdown_blocking();
}

#[test]
fn opt_in_epoch_captured_before_a_later_opt_out_cannot_resume() {
    let store = Arc::new(Store::default());
    let handle = TransportHandle::spawn(options(store));
    let generation = handle.generation();
    handle.pause_and_purge();
    assert!(!handle.resume(generation));
    assert_eq!(
        handle.enqueue_checked(Event::new("blocked", "user")),
        EnqueueResult::Disabled
    );
    handle.shutdown_blocking();
}

#[test]
fn restored_events_count_against_admission_bound() {
    let store = Arc::new(Store::default());
    store
        .events
        .lock()
        .unwrap()
        .push(Event::new("restored", "user"));
    let mut options = options(store);
    options.max_queue_size = 1;
    let handle = TransportHandle::spawn(options);
    assert_eq!(
        handle.enqueue_checked(Event::new("overflow", "user")),
        EnqueueResult::QueueFull
    );
    handle.shutdown_blocking();
}

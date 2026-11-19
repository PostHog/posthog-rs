//! Stateful queue coordination. Only this thread accesses event storage; HTTP
//! owns one bounded upload view and uses the shared response classifier.

use super::*;
use crate::client::event_store::StoreKey;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Entry {
    Disk(StoreKey),
    Memory(u64),
}

struct View {
    entries: Vec<(StoreKey, Entry)>,
    batch: RetryBatch,
}

pub(crate) struct AttemptResult {
    view: View,
    retry: bool,
}
struct Flush {
    completion: Completion,
    disk: u64,
    memory: u64,
}
impl Flush {
    fn new(completion: Completion, disk: u64, memory: u64) -> Self {
        Self {
            completion,
            disk,
            memory,
        }
    }
    fn precedes(&self, entry: Entry) -> bool {
        match entry {
            Entry::Disk(key) => key.0 > self.disk,
            Entry::Memory(id) => id > self.memory,
        }
    }
}
enum UploadJob {
    Analytics(View),
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    options: ClientOptions,
    lane: LaneConfig,
    rx: mpsc::Receiver<Control>,
    len: Arc<AtomicUsize>,
    clock: Arc<dyn Clock>,
    mut storage: StorageState,
    queue: Arc<memory::Queue>,
    deadline: Arc<Mutex<Option<Instant>>>,
    owned_workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    results: mpsc::Sender<Control>,
    ready: Option<mpsc::Sender<bool>>,
) {
    queue.persisted(
        storage
            .store
            .as_ref()
            .and_then(|store| store.end_key())
            .map_or(u64::MAX, |key| key.0),
    );
    let (jobs, uploads) = mpsc::sync_channel::<UploadJob>(1);
    let (startup, started) = mpsc::channel();
    let batch_bytes_target = lane.batch_bytes_target.unwrap_or(usize::MAX);
    let executor_options = options.clone();
    let executor_clock = clock.clone();
    let executor_deadline = deadline.clone();
    let persistent = storage.store.is_some();
    let executor = thread::Builder::new()
        .name("posthog-upload".into())
        .spawn(move || {
            let Some(mut pipeline) = Pipeline::new(
                &executor_options,
                lane,
                executor_clock,
                Arc::new(AtomicUsize::new(0)),
            ) else {
                let _ = startup.send(false);
                return;
            };
            pipeline.persistent_retention = persistent;
            pipeline.coordinated = true;
            let _ = pipeline.clock.now();
            let _ = startup.send(true);
            loop {
                let end = *executor_deadline.lock().unwrap_or_else(|p| p.into_inner());
                if end.is_some_and(|end| pipeline.clock.now() >= end) {
                    break;
                }
                let next = end;
                let wake = match next {
                    Some(at) => {
                        uploads.recv_timeout(at.saturating_duration_since(pipeline.clock.now()))
                    }
                    None => uploads
                        .recv()
                        .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
                };
                let job = match wake {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let end = *executor_deadline.lock().unwrap_or_else(|p| p.into_inner());
                        if end.is_some_and(|end| pipeline.clock.now() >= end) {
                            break;
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let end = *executor_deadline.lock().unwrap_or_else(|p| p.into_inner());
                if end.is_some_and(|end| pipeline.clock.now() >= end) {
                    break;
                }
                let mut view = match job {
                    UploadJob::Analytics(view) => {
                        if end.is_some_and(|end| pipeline.clock.now() >= end) {
                            continue;
                        }
                        view
                    }
                };
                pipeline
                    .len
                    .store(view.batch.pending.len(), Ordering::Release);
                let placeholder = RetryBatch {
                    pending: Vec::new(),
                    request_id: view.batch.request_id,
                    created_at: String::new(),
                    final_results: HashMap::new(),
                    historical_migration: false,
                    attempt: 1,
                    next_at: Instant::now(),
                };
                pipeline.attempt(std::mem::replace(&mut view.batch, placeholder), end);
                let retry = pipeline.retries.pop_front();
                if let Some(batch) = retry {
                    view.batch = batch;
                }
                if results
                    .send(Control::UploadComplete(Box::new(AttemptResult {
                        retry: !view.batch.pending.is_empty(),
                        view,
                    })))
                    .is_err()
                {
                    break;
                }
            }
        });
    let status = started.recv();
    if !matches!(status, Ok(true)) {
        if let Some(ready) = ready {
            if status.is_ok() {
                let _ = ready.send(false);
            }
        }
        drop(jobs);
        if let Ok(executor) = executor {
            let _ = executor.join();
        }
        drain_pending_completions(&rx, &len);
        return;
    }
    if let Ok(executor) = executor {
        owned_workers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(executor);
    }
    if let Some(ready) = ready {
        let _ = ready.send(true);
    }
    let interval = Duration::from_millis(options.flush_interval_ms.max(1));
    let mut due = if persistent {
        clock.now()
    } else {
        clock.now() + interval
    };
    let mut active = false;
    let mut waiting_for_executor = false;
    let mut completed: Option<AttemptResult> = None;
    let mut retry: Option<View> = None;
    let mut closing = false;
    let mut flushes: Vec<Flush> = Vec::new();
    let mut removals = Vec::new();
    let mut cleanup_due: Option<Instant> = None;
    let mut eviction_warned = false;
    let mut admitted_since_upload = 0usize;
    loop {
        let end = *deadline.lock().unwrap_or_else(|p| p.into_inner());
        if end.is_some_and(|end| clock.now() >= end) {
            break;
        }
        if let Some(result) = completed.take() {
            active = false;
            waiting_for_executor = false;
            let pending: std::collections::HashSet<_> = result
                .view
                .batch
                .pending
                .iter()
                .filter_map(|event| event.store_key)
                .collect();
            for (token, entry) in &result.view.entries {
                if !pending.contains(token) {
                    match entry {
                        Entry::Disk(key) => removals.push(*key),
                        Entry::Memory(id) => {
                            queue.remove(*id);
                        }
                    }
                }
            }
            if result.retry {
                retry = Some(result.view);
            }
            due = clock.now();
        }
        if queue.take_replaced() > 0 && !std::mem::replace(&mut eviction_warned, true) {
            tracing::warn!(
                "posthog-rs: event queue is full; dropping the oldest queued event(s) to keep new ones"
            );
        }
        if let Some(store) = &mut storage.store {
            if cleanup_due.map_or(true, |at| clock.now() >= at) {
                removals.retain(|key| {
                    if deadline
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .is_some_and(|end| clock.now() >= end)
                    {
                        return true;
                    }
                    !storage_support::acknowledge(store.as_mut(), *key, &options.on_error)
                });
                cleanup_due = (!removals.is_empty()).then(|| clock.now() + interval);
            }
        }
        // Persist staged suffix records even while the upload executor is blocked.
        let stage_end = queue.end();
        while let Some((item, can_persist)) = if persistent {
            queue.stage(stage_end)
        } else {
            None
        } {
            admitted_since_upload = admitted_since_upload.saturating_add(1);
            if deadline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_some_and(|end| clock.now() >= end)
            {
                break;
            }
            if can_persist && removals.is_empty() {
                if let Some(store) = &mut storage.store {
                    let key = storage_support::append(
                        store.as_mut(),
                        &item.wire,
                        StoreKey(item.id),
                        &options.on_error,
                        &mut eviction_warned,
                    );
                    if let Some(key) = key {
                        queue.persisted(key.0);
                        for flush in &mut flushes {
                            if item.id <= flush.memory {
                                flush.disk = flush.disk.max(key.0);
                            }
                        }
                        if !queue.remove(item.id) {
                            // An evicted in-progress append must never re-enter
                            // delivery, even when removing its new file fails.
                            removals.push(key);
                            cleanup_due = Some(clock.now());
                        }
                    }
                }
            }
        }
        if deadline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some_and(|end| clock.now() >= end)
        {
            break;
        }
        if !active && removals.is_empty() {
            if let Some(view) = &mut retry {
                let entries = &view.entries;
                view.batch.pending.retain(|event| {
                    let entry = entries
                        .iter()
                        .find(|(key, _)| Some(*key) == event.store_key)
                        .map(|(_, entry)| *entry);
                    match entry {
                        Some(Entry::Memory(id)) => queue.contains(id),
                        Some(Entry::Disk(key)) => storage
                            .store
                            .as_ref()
                            .is_some_and(|store| store.contains(key).unwrap_or(true)),
                        None => false,
                    }
                });
                if view.batch.pending.is_empty() {
                    retry = None;
                }
            }
            let eligible = retry
                .as_ref()
                .map_or(true, |view| clock.now() >= view.batch.next_at);
            if eligible
                && (closing
                    || !flushes.is_empty()
                    || admitted_since_upload >= options.flush_at.max(1)
                    || queue.len() >= options.flush_at.max(1)
                    || clock.now() >= due)
            {
                // A later waiter must not extend an earlier fixed checkpoint.
                let through = flushes.iter().map(|flush| flush.disk).min();
                let memory_through = flushes.iter().map(|flush| flush.memory).min();
                let mut scanned_more = false;
                let mut reselect_unbounded = false;
                let view = retry.take().or_else(|| {
                    let mut head_empty = true;
                    let mut events = Vec::new();
                    let mut entries = Vec::new();
                    if let Some(store) = &mut storage.store {
                        reselect_unbounded = through
                            .zip(store.end_key())
                            .is_some_and(|(end, key)| key.0 > end);
                        let count = options.max_batch_size.max(1).min(options.max_queue_size);
                        let bytes = queue.limit() as u64;
                        let target = batch_bytes_target.min(queue.limit()) as u64;
                        let page = match through {
                            Some(through) => {
                                store.head_batch_through(count, bytes, target, StoreKey(through))
                            }
                            None => store.head_batch(count, bytes, target),
                        };
                        match page {
                            Ok(page) => {
                                scanned_more = page.events.is_empty() && page.advanced && page.more;
                                for error in page.errors {
                                    super::super::common::report_storage_error(
                                        &options.on_error,
                                        crate::client::StorageOperation::Read,
                                        &error,
                                    );
                                }
                                head_empty =
                                    page.events.is_empty() && !page.more && !page.retry_pending;
                                for record in page.events {
                                    let token = StoreKey(events.len() as u64 + 1);
                                    let mut event = DeliveryEvent::from(record.event);
                                    event.store_key = Some(token);
                                    entries.push((token, Entry::Disk(record.key)));
                                    events.push(event);
                                }
                            }
                            Err(error) => {
                                super::super::common::report_storage_error(
                                    &options.on_error,
                                    crate::client::StorageOperation::Read,
                                    &error,
                                );
                                head_empty = false;
                            }
                        }
                    }
                    if head_empty {
                        // A bounded disk head says nothing about later durable
                        // records: resident selection must share its checkpoint.
                        for (id, wire) in queue
                            .view(
                                options.max_batch_size.max(1),
                                batch_bytes_target.min(queue.limit()),
                                persistent,
                            )
                            .into_iter()
                            .take_while(|(id, _)| {
                                let covered = memory_through.map_or(true, |end| *id <= end);
                                reselect_unbounded |= !covered;
                                covered
                            })
                        {
                            let token = StoreKey(events.len() as u64 + 1);
                            let mut event = DeliveryEvent::from(wire);
                            event.store_key = Some(token);
                            entries.push((token, Entry::Memory(id)));
                            events.push(event);
                        }
                    }
                    if head_empty && events.is_empty() {
                        // The queue drained, so a later overflow warns again.
                        eviction_warned = false;
                    }
                    let mut body_bytes = 0usize;
                    let keep = events
                        .iter()
                        .take_while(|event| {
                            let next = measure_total(event.as_ref());
                            if body_bytes > 0
                                && next > batch_bytes_target.saturating_sub(body_bytes)
                            {
                                return false;
                            }
                            body_bytes = body_bytes.saturating_add(next);
                            true
                        })
                        .count();
                    events.truncate(keep);
                    entries.truncate(keep);
                    if events.is_empty() {
                        return None;
                    }
                    Some(View {
                        entries,
                        batch: RetryBatch {
                            pending: events,
                            request_id: Uuid::now_v7(),
                            created_at: wire_timestamp(clock.now_utc()),
                            final_results: HashMap::new(),
                            historical_migration: false,
                            attempt: 1,
                            next_at: clock.now(),
                        },
                    })
                });
                if let Some(view) = view {
                    if let Some((_, first)) = view.entries.iter().find(|(token, _)| {
                        view.batch
                            .pending
                            .first()
                            .is_some_and(|event| event.store_key == Some(*token))
                    }) {
                        let mut index = 0;
                        while index < flushes.len() {
                            if flushes[index].precedes(*first) {
                                let flush = flushes.remove(index);
                                flush.completion.signal();
                            } else {
                                index += 1;
                            }
                        }
                    }
                    match jobs.try_send(UploadJob::Analytics(view)) {
                        Ok(()) => {
                            active = true;
                            admitted_since_upload = 0;
                        }
                        Err(mpsc::TrySendError::Full(UploadJob::Analytics(view))) => {
                            retry = Some(view);
                            waiting_for_executor = true;
                        }
                        Err(_) => break,
                    }
                } else if scanned_more {
                    // Continue recovery next turn, after servicing a queued
                    // control instead of monopolizing the coordinator mailbox.
                    due = clock.now();
                } else {
                    // Only this sweep's waiters are covered; later checkpoints
                    // require their own bounded selection before completion.
                    let mut index = 0;
                    while index < flushes.len() {
                        if through.is_some_and(|through| flushes[index].disk > through)
                            || memory_through.is_some_and(|end| flushes[index].memory > end)
                        {
                            index += 1;
                            continue;
                        }
                        let flush = flushes.remove(index);
                        flush.completion.signal();
                    }
                    if closing && flushes.is_empty() {
                        break;
                    }
                    // Resume the deferred selection against the normal disk
                    // head, without starting a new cycle for an exhausted head.
                    due = if !flushes.is_empty() || reselect_unbounded {
                        clock.now()
                    } else {
                        clock.now() + interval
                    };
                }
            } else if !eligible {
                for flush in flushes.drain(..) {
                    flush.completion.signal();
                }
                if closing {
                    break;
                }
            }
        }
        if !active && !removals.is_empty() {
            for flush in flushes.drain(..) {
                flush.completion.signal();
            }
            if closing {
                break;
            }
        }
        let next = if active || waiting_for_executor {
            end
        } else if !removals.is_empty() {
            cleanup_due.into_iter().chain(end).min()
        } else {
            Some(retry.as_ref().map_or(due, |view| view.batch.next_at))
                .into_iter()
                .chain(end)
                .min()
        };
        let wake = match next {
            Some(at) => rx.recv_timeout(at.saturating_duration_since(clock.now())),
            None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match wake {
            Ok(Control::QueueWake) => {
                queue.clear_wake();
                waiting_for_executor = false;
            }
            Ok(Control::UploadComplete(result)) => completed = Some(*result),
            Ok(Control::Capture { event }) => {
                dec_len(&len, 1);
                queue.admit(build_event_at(&event, clock.now_utc()));
            }
            Ok(Control::Prepared { event }) => {
                dec_len(&len, 1);
                queue.admit(event.wire);
            }
            Ok(Control::QueueFlush {
                completion,
                disk,
                memory,
            }) => flushes.push(Flush::new(completion, disk, memory)),
            Ok(Control::Flush(completion) | Control::FlushCaptures(completion)) => {
                let (disk, memory) = queue.checkpoint();
                flushes.push(Flush::new(completion, disk, memory));
            }
            Ok(Control::Shutdown { completion, .. }) => {
                closing = true;
                flushes.push(Flush::new(completion, u64::MAX, u64::MAX));
            }
            #[cfg(test)]
            Ok(Control::Tick(completion)) => {
                due = clock.now();
                let (disk, memory) = queue.checkpoint();
                flushes.push(Flush::new(completion, disk, memory));
            }
            Ok(Control::HistoricalBatch { events }) => dec_len(&len, events.len()),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    // Resident events at exit were never written to disk: memory fallback, or
    // staging the store did not reach before shutdown.
    let dropped = queue.clear();
    if dropped > 0 {
        tracing::warn!(
            "posthog-rs: shutdown stopped delivery; dropping {dropped} event(s) that were held only in memory"
        );
    }
    drop(retry);
    drop(completed);
    // No further work can access the event store after the coordinator exits.
    drop(storage);
    for flush in flushes {
        flush.completion.signal();
    }
    drop(jobs);
    drain_pending_completions(&rx, &len);
}

#[cfg(test)]
mod tests {
    use super::super::storage_tests::{wire, TestDirectory};
    use super::*;
    use crate::client::event_store::{
        EventStore, FileEventStore, Recovery, StorageDirectory, StoreLimits, StoreWriteError,
    };
    use std::io::{self, Read, Write};
    use std::net::{TcpListener, TcpStream};

    fn request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            assert_ne!(read, 0);
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:").map(str::trim))
                    .unwrap()
                    .parse()
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    return String::from_utf8(bytes[end + 4..].to_vec()).unwrap();
                }
            }
        }
    }
    struct Observed {
        store: FileEventStore,
        written: mpsc::Sender<String>,
        first_head: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
    }
    impl EventStore for Observed {
        fn recover(&mut self, count: usize, bytes: u64) -> io::Result<Recovery> {
            self.store.recover(count, bytes)
        }
        fn head(&mut self, count: usize, bytes: u64) -> io::Result<Recovery> {
            self.store.head(count, bytes)
        }
        fn head_batch(&mut self, count: usize, bytes: u64, target: u64) -> io::Result<Recovery> {
            if let Some((entered, release)) = self.first_head.take() {
                entered.send(()).unwrap();
                release.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            self.store.head_batch(count, bytes, target)
        }
        fn head_batch_through(
            &mut self,
            count: usize,
            bytes: u64,
            target: u64,
            through: StoreKey,
        ) -> io::Result<Recovery> {
            self.store.head_batch_through(count, bytes, target, through)
        }
        fn end_key(&self) -> Option<StoreKey> {
            self.store.end_key()
        }
        fn contains(&self, key: StoreKey) -> io::Result<bool> {
            self.store.contains(key)
        }
        fn put(&mut self, event: &CaptureEvent) -> Result<StoreKey, StoreWriteError> {
            let result = self.store.put(event);
            if result.is_ok() {
                self.written.send(event.event.clone()).unwrap();
            }
            result
        }
        fn put_at(
            &mut self,
            event: &CaptureEvent,
            key: StoreKey,
        ) -> Result<StoreKey, StoreWriteError> {
            let result = self.store.put_at(event, key);
            if result.is_ok() {
                self.written.send(event.event.clone()).unwrap();
            }
            result
        }
        fn remove(&mut self, key: StoreKey) -> io::Result<()> {
            self.store.remove(key)
        }
    }

    #[test]
    fn admission_during_a_head_read_is_uploaded_once_after_staging() {
        use httpmock::{Method::POST, MockServer};

        let directory = TestDirectory::new();
        let server = MockServer::start();
        let (attempted, attempts) = mpsc::channel();
        let (release_a, a_gate) = mpsc::channel();
        let a_gate = Mutex::new(Some(a_gate));
        let a_attempted = attempted.clone();
        server.mock(|when, then| {
            when.method(POST).body_includes("\"event\":\"A\"");
            then.respond_with(move |request: &httpmock::HttpMockRequest| {
                a_attempted.send("A").unwrap();
                if let Some(gate) = a_gate.lock().unwrap().take() {
                    gate.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                http::Response::builder()
                    .status(200)
                    .body(super::super::tests::accept_all(request))
                    .unwrap()
                    .into()
            });
        });
        server.mock(|when, then| {
            when.method(POST).body_includes("\"event\":\"B\"");
            then.respond_with(move |request: &httpmock::HttpMockRequest| {
                attempted.send("B").unwrap();
                http::Response::builder()
                    .status(200)
                    .body(super::super::tests::accept_all(request))
                    .unwrap()
                    .into()
            });
        });
        let options = super::super::tests::options(server.base_url())
            .flush_at(1usize)
            .max_batch_size(1usize)
            .build()
            .unwrap();
        let (head_entered, head) = mpsc::channel();
        let (release_head, head_gate) = mpsc::channel();
        let (written, writes) = mpsc::channel();
        let store = Observed {
            store: directory.open(),
            written,
            first_head: Some((head_entered, head_gate)),
        };
        let handle = TransportHandle::spawn_stateful(options, Some(Box::new(store)), 8192).unwrap();
        head.recv_timeout(Duration::from_secs(3)).unwrap();
        // The empty disk head is being read after the coordinator took its
        // staging snapshot. These two arrivals need a subsequent staging turn.
        assert_eq!(handle.enqueue_prepared(wire("A")), Admission::Accepted);
        assert_eq!(handle.enqueue_prepared(wire("B")), Admission::Accepted);
        let (complete, flushed) = mpsc::channel();
        assert!(handle.send_control(Control::Flush(Completion::Blocking(complete))));
        release_head.send(()).unwrap();
        // Keep A's request open until both writes happen, so a premature
        // in-memory upload cannot race its own durable duplicate away.
        assert_eq!(writes.recv_timeout(Duration::from_secs(3)).unwrap(), "A");
        assert_eq!(writes.recv_timeout(Duration::from_secs(3)).unwrap(), "B");
        assert!(matches!(flushed.try_recv(), Err(mpsc::TryRecvError::Empty)));
        release_a.send(()).unwrap();
        flushed.recv_timeout(Duration::from_secs(3)).unwrap();
        handle.close_blocking();
        assert!(directory.records().is_empty());
        assert_eq!(attempts.try_iter().collect::<Vec<_>>(), ["A", "B"]);
    }

    fn durable_fallback_checkpoint(checkpoint: &str) {
        use httpmock::{Method::POST, MockServer};

        let directory = TestDirectory::new();
        let store = FileEventStore::open(
            StorageDirectory::open(&directory.0).unwrap(),
            StoreLimits {
                max_events: 20,
                max_bytes: 64 * 1024,
                max_record_bytes: 1024,
            },
        )
        .unwrap();
        let server = MockServer::start();
        let (entered, requests) = mpsc::channel();
        let (returned, returns) = mpsc::channel();
        let names = if checkpoint == "C" {
            vec!["A", "B", "C", "D"]
        } else {
            vec!["A", "B", "C"]
        };
        let mut releases = HashMap::new();
        for &name in &names {
            let (release, gate) = mpsc::channel();
            releases.insert(name, release);
            let gate = Mutex::new(gate);
            let entered = entered.clone();
            let returned = returned.clone();
            server.mock(|when, then| {
                when.method(POST)
                    .body_includes(format!("\"event\":\"{name}\""));
                then.respond_with(move |request: &httpmock::HttpMockRequest| {
                    entered.send(name).unwrap();
                    gate.lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                    returned.send(name).unwrap();
                    http::Response::builder()
                        .status(200)
                        .body(super::super::tests::accept_all(request))
                        .unwrap()
                        .into()
                });
            });
        }
        let (written, writes) = mpsc::channel();
        let (failed, failures) = mpsc::channel();
        let options = super::super::tests::options(server.base_url())
            .flush_at(1usize)
            .max_batch_size(1usize)
            .max_queue_size(4usize)
            .max_capture_attempts(1u32)
            .on_error(move |failure| {
                if let PostHogError::Storage(error) = failure {
                    if matches!(error.operation(), crate::client::StorageOperation::Write) {
                        failed.send(()).unwrap();
                    }
                }
            })
            .build()
            .unwrap();
        let handle = TransportHandle::spawn_stateful(
            options,
            Some(Box::new(Observed {
                store,
                written,
                first_head: None,
            })),
            8192,
        )
        .unwrap();
        assert_eq!(handle.enqueue_prepared(wire("A")), Admission::Accepted);
        assert_eq!(writes.recv_timeout(Duration::from_secs(3)).unwrap(), "A");
        let mut order = vec![requests.recv_timeout(Duration::from_secs(3)).unwrap()];
        let (complete, flushed) = mpsc::channel();
        let mut complete = Some(complete);
        if checkpoint == "A" {
            assert!(handle.send_control(Control::Flush(Completion::Blocking(
                complete.take().unwrap()
            ))));
        }
        assert_eq!(handle.enqueue_prepared(wire("B")), Admission::Accepted);
        assert_eq!(writes.recv_timeout(Duration::from_secs(3)).unwrap(), "B");
        assert!(directory
            .0
            .join("events/00000000000000000002.json")
            .is_file());
        if checkpoint == "B" {
            assert!(handle.send_control(Control::Flush(Completion::Blocking(
                complete.take().unwrap()
            ))));
        }
        let mut fallback = wire("C");
        fallback.properties = serde_json::json!({"padding": "x".repeat(2048)});
        let bytes = crate::client::event_store::record_size(&fallback);
        assert!(bytes > 1024 && bytes < 8192);
        assert_eq!(handle.enqueue_prepared(fallback), Admission::Accepted);
        failures.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(!directory
            .0
            .join("events/00000000000000000003.json")
            .exists());
        let later_completion = if checkpoint == "C" {
            assert!(handle.send_control(Control::Flush(Completion::Blocking(
                complete.take().unwrap()
            ))));
            assert_eq!(handle.enqueue_prepared(wire("D")), Admission::Accepted);
            let (complete, completed) = mpsc::channel();
            assert!(handle.send_control(Control::Flush(Completion::Blocking(complete))));
            Some(completed)
        } else {
            None
        };
        let mut checkpoint_completed = None;
        let mut covered_work_pending = true;
        let mut later_pending = true;
        let mut normal_returns = Vec::new();
        let mut request_progress = Ok(());
        for index in 0..names.len() {
            let name = order[index];
            if name <= checkpoint {
                covered_work_pending &=
                    matches!(flushed.try_recv(), Err(mpsc::TryRecvError::Empty));
            } else if checkpoint_completed.is_none() {
                checkpoint_completed = Some(flushed.recv_timeout(Duration::from_secs(3)));
            }
            if let Some(later) = &later_completion {
                later_pending &= matches!(later.try_recv(), Err(mpsc::TryRecvError::Empty));
            }
            let _ = releases[name].send(());
            match returns.recv_timeout(Duration::from_secs(3)) {
                Ok(name) => normal_returns.push(name),
                Err(error) => {
                    request_progress = Err(error);
                    break;
                }
            }
            if index + 1 < names.len() {
                match requests.recv_timeout(Duration::from_secs(3)) {
                    Ok(name) => order.push(name),
                    Err(error) => {
                        request_progress = Err(error);
                        break;
                    }
                }
            }
        }
        for release in releases.values() {
            let _ = release.send(());
        }
        let completion =
            checkpoint_completed.unwrap_or_else(|| flushed.recv_timeout(Duration::from_secs(3)));
        let later_completed =
            later_completion.map(|later| later.recv_timeout(Duration::from_secs(3)));
        let (complete, drained) = mpsc::channel();
        assert!(handle.send_control(Control::Flush(Completion::Blocking(complete))));
        let drain = drained.recv_timeout(Duration::from_secs(3));
        handle.request_close_before(Instant::now());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !handle.all_stopped() {
            assert!(
                Instant::now() < deadline,
                "mixed fallback cleanup did not finish"
            );
            thread::yield_now();
        }
        handle.join();
        request_progress.expect("gated requests did not progress after explicit release");
        completion.expect("checkpoint waited for unrelated gated work");
        if let Some(later) = later_completed {
            later.expect("later fallback checkpoint did not complete");
        }
        drain.expect("final drain did not complete");
        assert!(
            covered_work_pending,
            "checkpoint skipped its covered fallback"
        );
        assert!(
            later_pending,
            "earlier exhaustion completed the later checkpoint"
        );
        assert_eq!(
            normal_returns, order,
            "gated callbacks did not return normally"
        );
        assert!(directory.records().is_empty());
        assert_eq!(order, names, "retained durable/fallback request FIFO");
    }

    #[test]
    fn checkpoint_exhaustion_preserves_post_checkpoint_durable_fallback_fifo() {
        durable_fallback_checkpoint("A");
    }

    #[test]
    fn nonempty_checkpoint_selection_preserves_later_fallback_fifo() {
        durable_fallback_checkpoint("B");
    }

    #[test]
    fn checkpoint_attempts_covered_fallback_before_later_resident_work() {
        durable_fallback_checkpoint("C");
    }

    #[test]
    fn memory_only_checkpoints_keep_distinct_resident_watermarks() {
        use httpmock::{Method::POST, MockServer};

        let server = MockServer::start();
        let (entered, requests) = mpsc::channel();
        let (returned, returns) = mpsc::channel();
        let mut releases = Vec::new();
        for name in ["A", "B"] {
            let (release, gate) = mpsc::channel();
            releases.push(release);
            let gate = Mutex::new(gate);
            let entered = entered.clone();
            let returned = returned.clone();
            server.mock(|when, then| {
                when.method(POST)
                    .body_includes(format!("\"event\":\"{name}\""));
                then.respond_with(move |request: &httpmock::HttpMockRequest| {
                    entered.send(name).unwrap();
                    gate.lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                    returned.send(name).unwrap();
                    http::Response::builder()
                        .status(200)
                        .body(super::super::tests::accept_all(request))
                        .unwrap()
                        .into()
                });
            });
        }
        let handle = TransportHandle::spawn_stateful(
            super::super::tests::options(server.base_url())
                .flush_at(1usize)
                .max_batch_size(1usize)
                .build()
                .unwrap(),
            None,
            8192,
        )
        .unwrap();
        assert_eq!(handle.enqueue_prepared(wire("A")), Admission::Accepted);
        let first = requests.recv_timeout(Duration::from_secs(3)).unwrap();
        let (complete, first_flush) = mpsc::channel();
        assert!(handle.send_control(Control::Flush(Completion::Blocking(complete))));
        assert_eq!(handle.enqueue_prepared(wire("B")), Admission::Accepted);
        let (complete, second_flush) = mpsc::channel();
        assert!(handle.send_control(Control::Flush(Completion::Blocking(complete))));
        releases[0].send(()).unwrap();
        assert_eq!(returns.recv_timeout(Duration::from_secs(3)).unwrap(), "A");
        let second = requests.recv_timeout(Duration::from_secs(3)).unwrap();
        let completed_first = first_flush.recv_timeout(Duration::from_secs(3));
        let second_pending = matches!(second_flush.try_recv(), Err(mpsc::TryRecvError::Empty));
        releases[1].send(()).unwrap();
        assert_eq!(returns.recv_timeout(Duration::from_secs(3)).unwrap(), "B");
        let completed_second = second_flush.recv_timeout(Duration::from_secs(3));
        handle.request_close_before(Instant::now());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !handle.all_stopped() {
            assert!(
                Instant::now() < deadline,
                "resident checkpoint cleanup did not finish"
            );
            thread::yield_now();
        }
        handle.join();
        completed_first.expect("earlier resident checkpoint waited for later work");
        assert!(
            second_pending,
            "earlier exhaustion completed a later resident checkpoint"
        );
        completed_second.expect("later resident checkpoint did not complete");
        assert_eq!([first, second], ["A", "B"]);
    }

    #[derive(Clone, Copy)]
    enum RolloverCase {
        Flush,
        LaterArrival,
        LaterFlush,
        Shutdown,
    }

    fn recovery_flush_rollover(case: RolloverCase) {
        use httpmock::{Method::POST, MockServer};
        use std::fs;

        let later_arrival = matches!(case, RolloverCase::LaterArrival | RolloverCase::LaterFlush);

        let directory = TestDirectory::new();
        let limits = StoreLimits {
            max_events: 20,
            max_bytes: 128 * 1024,
            max_record_bytes: 16 * 1024,
        };
        {
            let mut store =
                FileEventStore::open(StorageDirectory::open(&directory.0).unwrap(), limits)
                    .unwrap();
            let mut oversized = wire("preserved");
            oversized.properties = serde_json::json!({"padding": "x".repeat(8192)});
            for _ in 0..3 {
                store.put(&oversized).unwrap();
            }
        }
        let preserved: Vec<_> = directory
            .records()
            .iter()
            .map(|path| fs::read(path).unwrap())
            .collect();
        let store = FileEventStore::open(
            StorageDirectory::open(&directory.0).unwrap(),
            StoreLimits {
                max_record_bytes: 4096,
                ..limits
            },
        )
        .unwrap();
        let server = MockServer::start();
        let (attempted, attempts) = mpsc::channel();
        server.mock(|when, then| {
            when.method(POST).body_includes("before-checkpoint");
            then.respond_with(move |request: &httpmock::HttpMockRequest| {
                attempted.send(()).unwrap();
                http::Response::builder()
                    .status(200)
                    .body(super::super::tests::accept_all(request))
                    .unwrap()
                    .into()
            });
        });
        let (release_later, later_gate) = mpsc::channel();
        let later_gate = Mutex::new(later_gate);
        let (later_returned, later_returns) = mpsc::channel();
        let (later_entered, later_started) = mpsc::channel();
        server.mock(|when, then| {
            when.method(POST).body_includes("after-checkpoint");
            then.respond_with(move |request: &httpmock::HttpMockRequest| {
                later_entered.send(()).unwrap();
                later_gate
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                later_returned.send(()).unwrap();
                http::Response::builder()
                    .status(200)
                    .body(super::super::tests::accept_all(request))
                    .unwrap()
                    .into()
            });
        });
        let (page_entered, pages) = mpsc::channel();
        let (release_page, page_gate) = mpsc::channel();
        let page_gate = Mutex::new(page_gate);
        let (page_returned, page_returns) = mpsc::channel();
        let reads = AtomicUsize::new(0);
        let options = super::super::tests::options(server.base_url())
            .flush_at(20usize)
            .flush_interval_ms(3_600_000u64)
            .max_queue_size(1usize)
            .max_batch_size(1usize)
            .on_error(move |failure| {
                if let PostHogError::Storage(error) = failure {
                    if matches!(error.operation(), crate::client::StorageOperation::Read) {
                        let page = reads.fetch_add(1, Ordering::SeqCst) + 1;
                        if page <= 2 {
                            page_entered.send(page).unwrap();
                            page_gate
                                .lock()
                                .unwrap()
                                .recv_timeout(Duration::from_secs(5))
                                .unwrap();
                            page_returned.send(page).unwrap();
                        }
                    }
                }
            })
            .build()
            .unwrap();
        let handle = TransportHandle::spawn_stateful(options, Some(Box::new(store)), 4096).unwrap();
        assert_eq!(pages.recv_timeout(Duration::from_secs(3)).unwrap(), 1);
        assert_eq!(
            handle.enqueue_prepared(wire("before-checkpoint")),
            Admission::Accepted
        );
        let (complete, flushed) = mpsc::channel();
        if matches!(case, RolloverCase::Shutdown) {
            assert!(handle.request_shutdown(Completion::Blocking(complete)));
        } else {
            assert!(handle.send_control(Control::Flush(Completion::Blocking(complete))));
        }
        release_page.send(()).unwrap();
        assert_eq!(
            page_returns.recv_timeout(Duration::from_secs(3)).unwrap(),
            1
        );
        assert_eq!(pages.recv_timeout(Duration::from_secs(3)).unwrap(), 2);
        // QueueWake precedes QueueFlush. The second page is reached only after
        // the admitted record has been durably staged under its real key.
        let before = directory.0.join("events/00000000000000000004.json");
        assert!(before.is_file());
        if later_arrival {
            assert_eq!(
                handle.enqueue_prepared(wire("after-checkpoint")),
                Admission::Accepted
            );
        }
        let later_completion = if matches!(case, RolloverCase::LaterFlush) {
            let (complete, completed) = mpsc::channel();
            assert!(handle.send_control(Control::Flush(Completion::Blocking(complete))));
            Some(completed)
        } else {
            None
        };
        release_page.send(()).unwrap();
        assert_eq!(
            page_returns.recv_timeout(Duration::from_secs(3)).unwrap(),
            2
        );
        let completion = flushed.recv_timeout(Duration::from_secs(3));
        let attempted_before_completion = attempts.try_recv().is_ok();
        let later_attempt = later_completion
            .as_ref()
            .map(|_| later_started.recv_timeout(Duration::from_secs(3)));
        let later_flush_unfinished = later_completion
            .as_ref()
            .map_or(true, |completed| completed.try_recv().is_err());
        let later_unfinished = later_returns.try_recv().is_err();
        // Preserve the observation before cleanup, including a premature flush.
        handle.request_close_before(Instant::now());
        let _ = release_later.send(());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !handle.all_stopped() {
            assert!(Instant::now() < deadline, "rollover cleanup did not finish");
            thread::yield_now();
        }
        handle.join();
        completion.expect("flush did not finish through its fixed checkpoint");
        assert!(
            attempted_before_completion,
            "flush completed before attempting its pre-checkpoint record"
        );
        if let Some(later_attempt) = later_attempt {
            later_attempt.expect("later checkpoint did not reach its own delivery attempt");
            later_returns
                .recv_timeout(Duration::from_secs(3))
                .expect("later request did not return normally after explicit release");
        }
        assert!(
            later_flush_unfinished,
            "earlier scan completed a later flush checkpoint"
        );
        assert!(
            later_unfinished,
            "post-checkpoint work unexpectedly finished before release"
        );
        assert!(!before.exists());
        let retained = directory.records();
        assert_eq!(retained.len(), 3 + usize::from(later_arrival));
        for (path, original) in retained.iter().zip(&preserved) {
            assert_eq!(fs::read(path).unwrap(), *original);
        }
    }

    #[test]
    fn flush_rollover_attempts_the_record_appended_after_its_old_scan_started() {
        recovery_flush_rollover(RolloverCase::Flush);
    }

    #[test]
    fn flush_rollover_does_not_wait_for_a_post_checkpoint_arrival() {
        recovery_flush_rollover(RolloverCase::LaterArrival);
    }

    #[test]
    fn flush_rollover_completes_each_checkpoint_independently() {
        recovery_flush_rollover(RolloverCase::LaterFlush);
    }

    #[test]
    fn shutdown_rollover_attempts_admitted_records_before_exiting() {
        recovery_flush_rollover(RolloverCase::Shutdown);
    }

    #[test]
    fn blocked_upload_allows_persistence_exact_eviction_and_fixed_flush_checkpoint() {
        for status in [200, 503] {
            let directory = TestDirectory::new();
            let store = FileEventStore::open(
                StorageDirectory::open(&directory.0).unwrap(),
                StoreLimits {
                    max_events: 1,
                    max_bytes: 4096,
                    max_record_bytes: 4096,
                },
            )
            .unwrap();
            let (written, writes) = mpsc::channel();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let host = format!("http://{}", listener.local_addr().unwrap());
            let (entered, upload) = mpsc::channel();
            let (release, gate) = mpsc::channel();
            let (finish_second, second_gate) = mpsc::channel();
            let server = thread::spawn(move || {
                let (mut first, _) = listener.accept().unwrap();
                let body = request(&mut first);
                let reply = if status == 200 {
                    super::super::tests::accept_all_body(body.as_bytes())
                } else {
                    "{}".to_owned()
                };
                entered.send(body).unwrap();
                gate.recv_timeout(Duration::from_secs(3)).unwrap();
                write!(
                    first,
                    "HTTP/1.1 {} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    reply.len(),
                    reply
                )
                .unwrap();
                drop(first);
                let (mut second, _) = listener.accept().unwrap();
                let body = request(&mut second);
                let reply = super::super::tests::accept_all_body(body.as_bytes());
                entered.send(body).unwrap();
                second_gate.recv_timeout(Duration::from_secs(3)).unwrap();
                write!(
                    second,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    reply.len(),
                    reply
                )
                .unwrap();
            });
            let options = super::super::tests::options(host)
                .flush_at(1usize)
                .max_batch_size(1usize)
                .max_queue_size(1usize)
                .shutdown_timeout_ms(100u64)
                .max_capture_attempts(1u32)
                .build()
                .unwrap();
            let handle = TransportHandle::spawn_stateful(
                options,
                Some(Box::new(Observed {
                    store,
                    written,
                    first_head: None,
                })),
                4096,
            )
            .unwrap();
            let first = wire("evicted-first");
            let mut second = wire("retained-second");
            second.uuid = first.uuid;
            assert_eq!(handle.enqueue_prepared(first), Admission::Accepted);
            assert_eq!(
                writes.recv_timeout(Duration::from_secs(3)).unwrap(),
                "evicted-first"
            );
            assert!(upload
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .contains("evicted-first"));
            let (checkpoint, flushed) = mpsc::channel();
            assert!(handle.send_control(Control::Flush(Completion::Blocking(checkpoint))));
            assert_eq!(handle.enqueue_prepared(second), Admission::Accepted);
            let persisted = writes.recv_timeout(Duration::from_secs(3));
            release.send(()).unwrap();
            assert_eq!(
                persisted.unwrap(),
                "retained-second",
                "persistence must progress before HTTP release"
            );
            let replacement = upload.recv_timeout(Duration::from_secs(3)).unwrap();
            assert!(replacement.contains("retained-second"));
            assert!(!replacement.contains("evicted-first"));
            let completed_checkpoint = flushed.recv_timeout(Duration::from_secs(3));
            finish_second.send(()).unwrap();
            completed_checkpoint.expect("flush waited for an upload admitted after its checkpoint");
            handle.flush_blocking();
            server.join().unwrap();
            assert!(directory.records().is_empty());
            handle.close_blocking();
        }
    }
}

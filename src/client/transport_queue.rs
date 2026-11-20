//! The stateful delivery coordinator and its upload executor.
//!
//! One coordinator thread owns every delivery decision. It moves admitted
//! events from the inbox into the [`Table`], writes them to the store, picks
//! the next request, applies each response, retries, completes flushes, and
//! makes the one shutdown decision. The executor thread sends each request,
//! reads capture's verdicts and reports failures through `on_error`, then
//! hands back which events are done and which to retry. A slow upload or a
//! slow capture hook never blocks persistence.
//!
//! Delivery state lives in one place: an event is queued exactly while it has
//! an entry in the table. Storage failures follow the iOS and Android queues:
//! an event whose write fails is dropped, a record that cannot be read is
//! dropped and its file deleted, and a file that cannot be deleted is left
//! behind and picked up again after a restart.

use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::AtomicUsize;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::warn;
use uuid::Uuid;

use super::inbox::{Inbox, Waiting};
use super::table::{Entry, Limits, Table};
use super::{
    apply_on_error_hooks, bound_request, dec_len, drain_pending_completions, undelivered_results,
    undelivered_totals, Clock, Completion, Control, LaneConfig,
};
use crate::capture_event::{
    wire_timestamp, BatchRequestRef, CaptureErrorResponse, CaptureEvent, EventResult,
};
use crate::client::capture::{self, Step};
use crate::client::event_store::{EventStore, StoreKey};
use crate::client::{CaptureFailure, ClientOptions, PostHogError, StorageOperation};
use crate::endpoints::Endpoint;
use crate::error::Error;

/// The largest capture response body read, as in posthog-android. A larger
/// body is unreadable.
const MAX_RESPONSE_BODY_BYTES: u64 = 1024 * 1024;

/// One event in a request, tagged with its queue position.
pub(crate) struct Sending {
    seq: u64,
    event: CaptureEvent,
}

impl AsRef<CaptureEvent> for Sending {
    fn as_ref(&self) -> &CaptureEvent {
        &self.event
    }
}

impl AsMut<CaptureEvent> for Sending {
    fn as_mut(&mut self) -> &mut CaptureEvent {
        &mut self.event
    }
}

/// A request for the executor to send.
pub(crate) struct Job {
    request_id: Uuid,
    attempt: u32,
    created_at: String,
    events: Vec<Sending>,
}

/// A sent request and what is left of it.
pub(crate) struct Sent {
    seqs: Vec<u64>,
    request_id: Uuid,
    attempt: u32,
    created_at: String,
    verdict: Verdict,
}

enum Verdict {
    /// Every event reached a final outcome: delivered, or reported lost.
    Finished,
    /// These events, with any duplicate copies capture dropped, are retried
    /// after `delay`. The rest of the request is finished.
    Retry {
        retained: Vec<(u64, u32)>,
        delay: Duration,
    },
}

enum Outcome {
    /// No response was read: a network error or timeout.
    Network(String),
    Response {
        status: u16,
        retry_after: Option<Duration>,
        body: String,
    },
    /// The request body could not be built.
    Unsendable(String),
}

/// A request waiting to be sent again after a retryable outcome.
struct Retry {
    seqs: Vec<u64>,
    request_id: Uuid,
    attempt: u32,
    created_at: String,
    next_at: Instant,
}

struct Waiter {
    /// Covers every event at or below this queue position.
    through: u64,
    completion: Completion,
}

pub(super) struct Settings {
    pub(super) options: ClientOptions,
    pub(super) lane: LaneConfig,
    pub(super) memory: Limits,
}

pub(super) struct Coordinator {
    options: ClientOptions,
    endpoint: Endpoint,
    batch_bytes_target: u64,
    interval: Duration,
    /// The disk limits with a store, the memory limits without one.
    limits: Limits,
    clock: Arc<dyn Clock>,
    pending: Arc<AtomicUsize>,
    inbox: Arc<Inbox>,
    store: Option<Box<dyn EventStore>>,
    table: Table,
    deadline: Arc<Mutex<Option<Instant>>>,
    jobs: Option<mpsc::SyncSender<Job>>,
    /// A request is with the executor.
    sending: bool,
    retry: Option<Retry>,
    /// When the next request is due without a flush or a full batch.
    due: Option<Instant>,
    /// The newest queue position when the last request was selected. Entries
    /// at or below it that remain once it is answered are a backlog.
    selected_through: u64,
    waiters: Vec<Waiter>,
    shutdown: Option<Completion>,
    capacity_warned: bool,
    write_failing: bool,
    /// Duplicate copies capture dropped for an event that is being retried,
    /// reported when the event reaches its final outcome.
    copies: HashMap<u64, u32>,
}

impl Coordinator {
    /// Build the queue from the records already on disk. Runs on the caller's
    /// thread, before admission starts, so new events follow the last record.
    pub(super) fn new(
        settings: Settings,
        mut store: Option<Box<dyn EventStore>>,
        clock: Arc<dyn Clock>,
        pending: Arc<AtomicUsize>,
        deadline: Arc<Mutex<Option<Instant>>>,
    ) -> (Self, Arc<Inbox>) {
        let mut table = Table::default();
        let mut last_seq = 0;
        if let Some(store) = &mut store {
            // New events follow every record file, so none reuses a key.
            last_seq = store.last_key().0;
            for (key, bytes) in store.take_index() {
                table.push(Entry {
                    seq: key.0,
                    bytes,
                    payload: None,
                });
            }
        }
        let inbox = Arc::new(Inbox::new(
            last_seq,
            settings.memory.events,
            settings.memory.bytes as usize,
            pending.clone(),
        ));
        let now = clock.now();
        let interval = Duration::from_millis(settings.options.flush_interval_ms.max(1));
        let limits = store.as_ref().map_or(settings.memory, |store| {
            let limits = store.limits();
            Limits {
                events: limits.max_events,
                bytes: limits.max_bytes,
            }
        });
        let coordinator = Self {
            endpoint: settings.lane.endpoint,
            batch_bytes_target: settings
                .lane
                .batch_bytes_target
                .unwrap_or(usize::MAX)
                .min(settings.memory.bytes as usize) as u64,
            interval,
            limits,
            options: settings.options,
            clock,
            pending,
            inbox: inbox.clone(),
            store,
            // A backlog from an earlier run is sent at once.
            due: (!table.is_empty()).then_some(now),
            selected_through: 0,
            table,
            deadline,
            jobs: None,
            sending: false,
            retry: None,
            waiters: Vec::new(),
            shutdown: None,
            capacity_warned: false,
            write_failing: false,
            copies: HashMap::new(),
        };
        (coordinator, inbox)
    }

    pub(super) fn run(mut self, jobs: mpsc::SyncSender<Job>, rx: mpsc::Receiver<Control>) {
        self.jobs = Some(jobs);
        loop {
            if self.past_deadline() {
                break;
            }
            self.drain_inbox();
            self.evict_over_capacity();
            if !self.sending && !self.start_request() {
                break;
            }
            self.complete_waiters();
            if self.shutdown.is_some() && self.drained() {
                break;
            }
            let message = match self.next_wake() {
                Some(at) => rx.recv_timeout(at.saturating_duration_since(self.clock.now())),
                None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            };
            match message {
                Ok(control) => self.handle(control),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        self.finish(&rx);
    }

    fn handle(&mut self, control: Control) {
        match control {
            Control::QueueWake => self.inbox.clear_wake(),
            Control::Sent(sent) => self.apply(*sent),
            Control::QueueFlush {
                completion,
                through,
            } => self.waiters.push(Waiter {
                through,
                completion,
            }),
            Control::Flush(completion) | Control::FlushCaptures(completion) => {
                self.waiters.push(Waiter {
                    through: self.inbox.last_seq(),
                    completion,
                })
            }
            Control::Shutdown(completion) => {
                let mut deadline = self.deadline.lock().unwrap_or_else(|p| p.into_inner());
                if deadline.is_none() {
                    let timeout = Duration::from_millis(self.options.shutdown_timeout_ms)
                        .min(super::MAX_SHUTDOWN_TIMEOUT);
                    *deadline = Some(self.clock.now() + timeout);
                }
                drop(deadline);
                if let Some(earlier) = self.shutdown.replace(completion) {
                    earlier.signal();
                }
            }
            #[cfg(test)]
            Control::Tick(completion) => {
                self.due = Some(self.clock.now());
                self.waiters.push(Waiter {
                    through: self.inbox.last_seq(),
                    completion,
                });
            }
            // The stateless worker's own controls never reach a coordinator.
            Control::Capture { .. } | Control::HistoricalBatch { .. } => {}
        }
    }

    fn drain_inbox(&mut self) {
        let waiting = self.inbox.drain();
        self.enqueue(waiting);
    }

    /// Move admitted events into the queue: to disk when there is a store,
    /// otherwise held in memory. An event whose write fails is dropped.
    fn enqueue(&mut self, waiting: Vec<Waiting>) {
        let now = self.clock.now();
        for waiting in waiting {
            let entry = if self.store.is_some() {
                // Written or dropped, the event no longer waits in memory.
                dec_len(&self.pending, 1);
                let Some(bytes) = self.write(waiting.seq, &waiting.event) else {
                    continue;
                };
                Entry {
                    seq: waiting.seq,
                    bytes,
                    payload: None,
                }
            } else {
                Entry {
                    seq: waiting.seq,
                    bytes: waiting.bytes as u64,
                    payload: Some(Arc::new(waiting.event)),
                }
            };
            self.table.push(entry);
            if self.due.is_none() {
                self.due = Some(now + self.interval);
            }
        }
    }

    /// Write an event to disk and return the record size, or `None` when the
    /// write failed and the event is dropped.
    fn write(&mut self, seq: u64, event: &CaptureEvent) -> Option<u64> {
        match self.store.as_mut()?.write(StoreKey(seq), event) {
            Ok(bytes) => {
                self.write_failing = false;
                Some(bytes)
            }
            Err(error) => {
                // One report per run of failures, not one per event.
                if !std::mem::replace(&mut self.write_failing, true) {
                    self.report_storage(StorageOperation::Write, &error);
                }
                None
            }
        }
    }

    /// Drop the oldest events over the queue limits, warning once per overflow
    /// until the queue drains.
    fn evict_over_capacity(&mut self) {
        let evicted = self.table.evict(self.limits);
        let dropped = self.inbox.take_dropped() + evicted.len();
        self.discard(evicted);
        if dropped > 0 && !std::mem::replace(&mut self.capacity_warned, true) {
            warn!(
                "posthog-rs: event queue for {} is full; dropping the oldest queued event(s) to keep new ones",
                self.endpoint
            );
        }
    }

    /// Release entries that left the queue: delete their files, or count
    /// memory-only ones out of the pending total.
    fn discard(&mut self, entries: Vec<Entry>) {
        for entry in entries {
            self.copies.remove(&entry.seq);
            if entry.on_disk() {
                self.delete(StoreKey(entry.seq));
            } else {
                dec_len(&self.pending, 1);
            }
        }
    }

    /// Delete a record. A file that cannot be deleted is reported and left
    /// behind; it is sent again after a restart.
    fn delete(&mut self, key: StoreKey) {
        let Some(store) = &mut self.store else {
            return;
        };
        if let Err(error) = store.delete(key) {
            self.report_storage(StorageOperation::Remove, &error);
        }
    }

    /// Hand the next request to the executor when one is due. Returns false
    /// once the executor has stopped.
    fn start_request(&mut self) -> bool {
        let now = self.clock.now();
        if let Some(retry) = &self.retry {
            if now < retry.next_at {
                return true;
            }
            let retry = self.retry.take().expect("checked above");
            // A retry keeps its identity only while all its events are queued.
            if self.table.contains_all(&retry.seqs) {
                return self.send(retry.seqs.clone(), Some(retry));
            }
            self.due = Some(now);
        }
        if self.table.is_empty() {
            self.due = None;
            return true;
        }
        let requested = self.shutdown.is_some()
            || self
                .waiters
                .iter()
                .any(|waiter| self.table.has_through(waiter.through));
        let due = self.due.is_some_and(|due| now >= due)
            || self.table.len() >= self.options.flush_at.max(1);
        if !requested && !due {
            return true;
        }
        let seqs = self
            .table
            .select(self.options.max_batch_size.max(1), self.batch_bytes_target);
        self.send(seqs, None)
    }

    /// Send `seqs`, as a new request or as `retry`. Returns false once the
    /// executor has stopped.
    fn send(&mut self, seqs: Vec<u64>, retry: Option<Retry>) -> bool {
        let mut events = Vec::with_capacity(seqs.len());
        for seq in &seqs {
            let Some(entry) = self.table.get(*seq) else {
                continue;
            };
            let event = match &entry.payload {
                Some(payload) => (**payload).clone(),
                None => {
                    let read = self
                        .store
                        .as_mut()
                        .expect("an on-disk entry has a store")
                        .read(StoreKey(*seq));
                    match read {
                        Ok(event) => event,
                        Err(error) => {
                            // Never sent: dropped from the queue and deleted.
                            self.report_storage(StorageOperation::Read, &error);
                            let removed = self.table.remove(&[*seq]);
                            self.discard(removed);
                            continue;
                        }
                    }
                }
            };
            let mut sending = Sending { seq: *seq, event };
            if let Some(copies) = self.copies.get(seq) {
                sending.event.dropped_copies = *copies;
            }
            events.push(sending);
        }
        if events.is_empty() {
            // Go on with whatever is still queued. An emptied queue keeps no
            // due time, so the next event waits for its flush interval.
            self.due = (!self.table.is_empty()).then(|| self.clock.now());
            return true;
        }
        // A retry keeps its identity only while it sends all its events.
        let identity = match retry {
            Some(retry) if events.len() == seqs.len() => {
                (retry.request_id, retry.attempt, retry.created_at)
            }
            _ => (Uuid::now_v7(), 1, wire_timestamp(self.clock.now_utc())),
        };
        let (request_id, attempt, created_at) = identity;
        let job = Job {
            request_id,
            attempt,
            created_at,
            events,
        };
        match self.jobs.as_ref().map(|jobs| jobs.try_send(job)) {
            Some(Ok(())) => {
                self.sending = true;
                // This request uses up the due time: events admitted while it
                // is out start their own interval.
                self.due = None;
                self.selected_through = self.table.last_seq().unwrap_or(0);
                true
            }
            // Only one request is ever outstanding, so the slot is free.
            Some(Err(mpsc::TrySendError::Full(_))) => unreachable!("one request at a time"),
            _ => false,
        }
    }

    /// Apply the outcome of a request: remove what finished and keep what
    /// will be retried.
    fn apply(&mut self, sent: Sent) {
        self.sending = false;
        match sent.verdict {
            Verdict::Finished => self.finish_events(&sent.seqs),
            Verdict::Retry { retained, delay } => {
                for (seq, copies) in &retained {
                    // Capacity may have evicted the event while it was out.
                    if *copies > 0 && self.table.get(*seq).is_some() {
                        self.copies.insert(*seq, *copies);
                    }
                }
                let retained: Vec<u64> = retained.into_iter().map(|(seq, _)| seq).collect();
                let done: Vec<u64> = sent
                    .seqs
                    .iter()
                    .copied()
                    .filter(|seq| !retained.contains(seq))
                    .collect();
                self.finish_events(&done);
                self.retry = Some(Retry {
                    seqs: retained,
                    request_id: sent.request_id,
                    attempt: sent.attempt.saturating_add(1),
                    created_at: sent.created_at,
                    next_at: self.clock.now() + delay,
                });
            }
        }
        // Events that were queued when the request was selected but did not go
        // in it are a backlog: send them now. This includes events admitted
        // during a retry wait, once the retry clears.
        if self.retry.is_none() && self.table.has_through(self.selected_through) {
            self.due = Some(self.clock.now());
        }
    }

    /// Remove events that reached a final outcome.
    fn finish_events(&mut self, seqs: &[u64]) {
        let removed = self.table.remove(seqs);
        self.discard(removed);
        if self.table.is_empty() {
            self.capacity_warned = false;
        }
    }

    fn complete_waiters(&mut self) {
        let retrying = self.retry.is_some();
        let table = &self.table;
        // A request waiting to be retried has been attempted, and holds back
        // everything after it. Events in flight stay in the table until their
        // answer is applied, so a waiter does not complete before them, unless
        // capacity evicted them first: they are then no longer queued, and a
        // flush does not wait for them.
        let (done, waiting): (Vec<_>, Vec<_>) = self
            .waiters
            .drain(..)
            .partition(|waiter| retrying || !table.has_through(waiter.through));
        self.waiters = waiting;
        for waiter in done {
            waiter.completion.signal();
        }
    }

    /// Whether shutdown can stop now: nothing is being sent, and nothing more
    /// can be sent before the deadline.
    fn drained(&self) -> bool {
        if self.sending {
            return false;
        }
        match &self.retry {
            Some(retry) => self
                .current_deadline()
                .map_or(true, |end| retry.next_at > end),
            None => self.table.is_empty(),
        }
    }

    fn next_wake(&self) -> Option<Instant> {
        let next = if self.sending {
            None
        } else {
            match &self.retry {
                // Nothing else is sent before the retry.
                Some(retry) => Some(retry.next_at),
                None if !self.table.is_empty() => self.due,
                None => None,
            }
        };
        match (self.current_deadline(), next) {
            (Some(deadline), Some(next)) => Some(deadline.min(next)),
            (deadline, next) => deadline.or(next),
        }
    }

    fn current_deadline(&self) -> Option<Instant> {
        *self.deadline.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn past_deadline(&self) -> bool {
        self.current_deadline()
            .is_some_and(|end| self.clock.now() >= end)
    }

    /// Stop: close the inbox and queue its last events, written to disk when
    /// there is a store, within the queue limits as in normal operation. Then
    /// report once what delivery could not finish, release storage, and
    /// signal every waiter.
    fn finish(mut self, rx: &mpsc::Receiver<Control>) {
        let waiting = self.inbox.close_and_drain();
        self.enqueue(waiting);
        self.evict_over_capacity();
        let mut on_disk = 0;
        let mut dropped = 0;
        for entry in self.table.take_all() {
            if entry.on_disk() {
                on_disk += 1;
            } else {
                dec_len(&self.pending, 1);
                dropped += 1;
            }
        }
        if on_disk > 0 || dropped > 0 {
            let mut parts = Vec::new();
            if on_disk > 0 {
                parts.push(format!(
                    "{on_disk} event(s) stay on disk for the next start"
                ));
            }
            if dropped > 0 {
                parts.push(format!(
                    "{dropped} event(s) held only in memory are dropped"
                ));
            }
            warn!(
                "posthog-rs: delivery to {} stopped before finishing; {}",
                self.endpoint,
                parts.join(", ")
            );
        }
        // Release the namespace lease before anyone waiting can reopen it.
        self.store = None;
        self.jobs = None;
        for waiter in self.waiters.drain(..) {
            waiter.completion.signal();
        }
        if let Some(completion) = self.shutdown.take() {
            completion.signal();
        }
        drain_pending_completions(rx, &self.pending);
    }

    fn report_storage(&self, operation: StorageOperation, error: &std::io::Error) {
        crate::client::common::report_storage_error(&self.options.on_error, operation, error);
    }
}

/// The upload executor: sends one request at a time, reads capture's
/// verdicts, and reports failures. `on_error` hooks run here, off the
/// coordinator.
pub(super) struct Executor {
    pub(super) http: reqwest::blocking::Client,
    pub(super) options: ClientOptions,
    pub(super) lane: LaneConfig,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) deadline: Arc<Mutex<Option<Instant>>>,
}

impl Executor {
    pub(super) fn run(self, jobs: mpsc::Receiver<Job>, results: mpsc::Sender<Control>) {
        let url = self.options.endpoints().build_url(self.lane.endpoint);
        while let Ok(job) = jobs.recv() {
            let seqs: Vec<u64> = job.events.iter().map(|sending| sending.seq).collect();
            let (request_id, attempt, created_at) =
                (job.request_id, job.attempt, job.created_at.clone());
            // A panic must not strand the coordinator waiting for this answer.
            let sent =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.attempt(&url, job)))
                    .unwrap_or_else(|_| Sent {
                        verdict: Verdict::Retry {
                            retained: seqs.iter().map(|seq| (*seq, 0)).collect(),
                            delay: crate::client::retry::capture_backoff_duration(
                                &self.options,
                                attempt,
                                None,
                            ),
                        },
                        seqs,
                        request_id,
                        attempt,
                        created_at,
                    });
            if results.send(Control::Sent(Box::new(sent))).is_err() {
                break;
            }
        }
    }

    fn attempt(&self, url: &str, job: Job) -> Sent {
        let outcome = self.send(url, &job);
        let Job {
            request_id,
            attempt,
            created_at,
            events,
        } = job;
        let seqs: Vec<u64> = events.iter().map(|sending| sending.seq).collect();
        let count = events.len();
        let mut pending = events;
        let mut results = HashMap::new();
        // A client queue has no attempt limit, as the retry-queue spec allows:
        // an event is retried until it is delivered, evicted by the queue
        // limits, or left behind at shutdown.
        let is_final = false;
        let mut status = None;
        let mut error_body = None;
        let step = match outcome {
            Outcome::Network(message) => capture::transport_error_step(
                &self.options,
                &request_id,
                attempt,
                message,
                is_final,
            ),
            Outcome::Unsendable(message) => Step::Fail(Error::Serialization(message)),
            Outcome::Response {
                status: code,
                retry_after,
                body,
            } => {
                status = Some(code);
                let step = capture::response_step(
                    &self.options,
                    &request_id,
                    attempt,
                    code,
                    retry_after,
                    &body,
                    &mut pending,
                    &mut results,
                    is_final,
                );
                if !(200..=299).contains(&code) {
                    error_body = Some(body);
                }
                step
            }
        };
        let report = Report {
            request_id: &request_id,
            attempt,
            status,
            results: &results,
        };
        let endpoint = self.lane.endpoint;
        let verdict = match step {
            Step::Done => {
                let lost = pending.len() + undelivered_results(&results);
                if lost > 0 {
                    if self.options.on_error.is_empty() {
                        warn!(
                            "posthog-rs: {lost} event(s) not persisted by {endpoint}: {}",
                            undelivered_totals(&results)
                        );
                    } else {
                        self.report(&report, None, None, lost);
                    }
                }
                Verdict::Finished
            }
            Step::Fail(error) => {
                if self.options.on_error.is_empty() {
                    warn!("posthog-rs: dropping {count} event(s) for {endpoint}: {error}");
                } else {
                    let error_response = error_body
                        .as_deref()
                        .and_then(|body| serde_json::from_str::<CaptureErrorResponse>(body).ok());
                    let lost = pending.len() + undelivered_results(&results);
                    self.report(&report, Some(&error), error_response.as_ref(), lost);
                }
                Verdict::Finished
            }
            Step::Backoff(delay) => {
                let rejected = undelivered_results(&results);
                if rejected > 0 {
                    if self.options.on_error.is_empty() {
                        warn!("posthog-rs: {rejected} event(s) rejected by {endpoint}");
                    } else {
                        self.report(&report, None, None, rejected);
                    }
                }
                Verdict::Retry {
                    retained: pending
                        .iter()
                        .map(|sending| (sending.seq, sending.event.dropped_copies))
                        .collect(),
                    delay,
                }
            }
        };
        Sent {
            seqs,
            request_id,
            attempt,
            created_at,
            verdict,
        }
    }

    fn report(
        &self,
        report: &Report<'_>,
        error: Option<&Error>,
        error_response: Option<&CaptureErrorResponse>,
        event_count: usize,
    ) {
        let failure = PostHogError::Capture(CaptureFailure {
            endpoint: self.lane.endpoint,
            error,
            status: report.status,
            attempt: report.attempt,
            event_count,
            historical_migration: false,
            request_id: Some(report.request_id),
            results: report.results,
            error_response,
        });
        apply_on_error_hooks(&self.options.on_error, &failure);
    }

    fn send(&self, url: &str, job: &Job) -> Outcome {
        let batch: Vec<&CaptureEvent> = job.events.iter().map(|sending| &sending.event).collect();
        let request = BatchRequestRef {
            created_at: &job.created_at,
            historical_migration: None,
            batch: &batch,
        };
        let payload = match serde_json::to_vec(&request) {
            Ok(payload) => payload,
            Err(error) => return Outcome::Unsendable(error.to_string()),
        };
        let mut headers = capture::build_headers_at(
            &self.options,
            &job.request_id,
            job.attempt,
            self.clock.now_utc(),
        );
        let body =
            capture::maybe_compress(self.lane.compression(&self.options), &mut headers, payload);
        let deadline = *self.deadline.lock().unwrap_or_else(|p| p.into_inner());
        let request = bound_request(
            self.http.post(url).headers(headers).body(body),
            deadline,
            self.clock.now(),
            Duration::from_secs(self.options.request_timeout_seconds),
        );
        match request.send() {
            Err(error) => Outcome::Network(error.to_string()),
            Ok(response) => {
                let status = response.status().as_u16();
                let retry_after = capture::parse_retry_after(response.headers());
                let mut bytes = Vec::new();
                let read = response
                    .take(MAX_RESPONSE_BODY_BYTES + 1)
                    .read_to_end(&mut bytes);
                // An oversized or cut-off body is unreadable. A success then
                // has no results to read and is not resent, as the capture
                // spec requires: capture likely stored its events, and a
                // resend would store them twice. Any other status is the
                // answer by itself.
                let readable = read.is_ok() && bytes.len() as u64 <= MAX_RESPONSE_BODY_BYTES;
                Outcome::Response {
                    status,
                    retry_after,
                    body: if readable {
                        String::from_utf8_lossy(&bytes).into_owned()
                    } else {
                        String::new()
                    },
                }
            }
        }
    }
}

struct Report<'a> {
    request_id: &'a Uuid,
    attempt: u32,
    status: Option<u16>,
    results: &'a HashMap<Uuid, EventResult>,
}

#[cfg(test)]
#[path = "transport_queue_tests.rs"]
mod tests;

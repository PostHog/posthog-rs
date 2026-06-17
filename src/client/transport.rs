//! Runtime-independent event transport.
//!
//! A background **dispatcher** `std::thread` drains the control channel, batches
//! events, and schedules retries; it hands each built batch to a small pool of
//! **sender** threads that perform the **blocking** reqwest POST, so one slow or
//! stalled endpoint can't head-of-line block draining. Senders report each
//! outcome back to the dispatcher, which solely owns the retry queue and
//! schedule. Being plain threads with a blocking client (never tokio tasks) this
//! works for the async client, the blocking client, and — in a later change — a
//! `std::panic` hook with no runtime present.
//!
//! `capture()` becomes a non-blocking enqueue (`Control::Capture`). `flush()` and
//! `shutdown()` send a control message carrying a [`Completion`] the dispatcher
//! signals once the sends it dispatched have reported back, bridging the threads
//! to either an async (`oneshot`) or blocking (`mpsc`) caller without putting a
//! runtime in the worker.
//!
//! A [`Clock`] is injected so the interval timer, retry backoff, and v1 wire
//! timestamps are deterministic in tests (a `ManualClock` plus a test-only `Tick`
//! command drive the dispatcher with virtual time; barriers wait on real sender
//! outcomes — still no real sleeps).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use crossbeam_channel::{select, unbounded, Receiver, Sender};
use tracing::warn;

use super::ClientOptions;
use crate::Event;

/// Messages sent from producers (`capture`/`flush`/`shutdown`) to the worker.
pub(crate) enum Control {
    // Boxed so the channel node isn't sized to a whole Event on every flush/
    // shutdown message (clippy::large_enum_variant).
    Capture {
        event: Box<Event>,
        historical_migration: bool,
    },
    Flush(Completion),
    Shutdown(Completion),
    /// Test-only: re-evaluate the (virtual) clock and flush/retry whatever is now
    /// due, so interval and backoff timing can be driven without real sleeps.
    #[cfg(test)]
    Tick(Completion),
}

/// Completion signal handed to the worker so the caller can wait for a flush or
/// shutdown to finish. The worker calls [`Completion::signal`] without needing a
/// runtime — `oneshot::Sender::send` and `mpsc::Sender::send` are both runtime-free.
pub(crate) enum Completion {
    Blocking(mpsc::Sender<()>),
    #[cfg(feature = "async-client")]
    Async(tokio::sync::oneshot::Sender<()>),
}

impl Completion {
    fn signal(self) {
        match self {
            Completion::Blocking(tx) => {
                let _ = tx.send(());
            }
            #[cfg(feature = "async-client")]
            Completion::Async(tx) => {
                let _ = tx.send(());
            }
        }
    }
}

/// Source of time for the worker. Injected so tests can drive the interval
/// timer, retry backoff, and v1 wire timestamps deterministically.
pub(crate) trait Clock: Send + Sync + 'static {
    /// Monotonic time, for batching/retry scheduling.
    fn now(&self) -> Instant;
    /// Wall-clock time, for v1 `created_at` / event timestamps / request headers.
    /// Unused by the v0 pipeline, which takes its timestamp from the event itself.
    #[cfg_attr(not(feature = "capture-v1"), allow(dead_code))]
    fn now_utc(&self) -> DateTime<Utc>;
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Handle stored on the client. `&self` methods use the atomics/mutex so the
/// client can stay a plain field while `capture`/`flush`/`shutdown` take `&self`.
pub(crate) struct TransportHandle {
    tx: Sender<Control>,
    /// Pending `Capture` events not yet pulled by the worker. Gates the bounded queue.
    len: Arc<AtomicUsize>,
    /// Set once `shutdown`/`Drop` begins; blocks further enqueue and control sends.
    closed: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
    /// Latches the single "queue full" warning so a full queue doesn't spam logs.
    full_warned: AtomicBool,
    max_queue_size: usize,
    /// Shares the worker's clock; used to stamp capture (enqueue) time.
    clock: Arc<dyn Clock>,
}

impl TransportHandle {
    /// Spawn the worker with the real system clock.
    pub(crate) fn spawn(options: ClientOptions) -> Self {
        Self::spawn_with_clock(options, Arc::new(SystemClock))
    }

    fn spawn_with_clock(options: ClientOptions, clock: Arc<dyn Clock>) -> Self {
        let (tx, rx) = unbounded::<Control>();
        let len = Arc::new(AtomicUsize::new(0));
        let max_queue_size = options.max_queue_size;
        let worker_len = len.clone();
        let worker_clock = Arc::clone(&clock);
        let worker = thread::Builder::new()
            .name("posthog-transport".to_string())
            .spawn(move || run_worker(options, rx, worker_len, worker_clock))
            .ok();
        Self {
            tx,
            len,
            closed: AtomicBool::new(false),
            worker: Mutex::new(worker),
            full_warned: AtomicBool::new(false),
            max_queue_size,
            clock,
        }
    }

    /// Non-blocking enqueue. Drops (with a single warning) when the queue is full
    /// or the client is closed.
    pub(crate) fn enqueue(&self, mut event: Event, historical_migration: bool) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if !try_reserve(&self.len, self.max_queue_size, &self.full_warned) {
            return;
        }
        // Stamp capture (enqueue) time on the producer side so a batched or
        // retried event records when it occurred, not when it was finally sent.
        event.ensure_timestamp(self.clock.now_utc());
        if self
            .tx
            .send(Control::Capture {
                event: Box::new(event),
                historical_migration,
            })
            .is_err()
        {
            // Worker gone; release the slot we reserved.
            self.len.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Send a flush/shutdown control message. Returns `false` if closed or the
    /// worker is gone, so a caller's wait becomes a no-op instead of hanging.
    pub(crate) fn send_control(&self, control: Control) -> bool {
        self.tx.send(control).is_ok()
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Events accepted but not yet delivered or dropped: channel depth plus the
    /// worker's current batch buffer plus any batches held for retry. Only the
    /// `test-harness`-gated `Client::pending_events` and the unit tests read this.
    #[cfg(any(test, feature = "test-harness"))]
    pub(crate) fn pending(&self) -> usize {
        self.len.load(Ordering::Acquire)
    }

    /// Mark closed. Returns `true` for the caller that won the transition (so
    /// shutdown is idempotent and only one caller drives teardown).
    pub(crate) fn begin_close(&self) -> bool {
        !self.closed.swap(true, Ordering::AcqRel)
    }

    /// Join the worker thread. Safe to call repeatedly.
    pub(crate) fn join(&self) {
        if let Some(handle) = self.worker.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = handle.join();
        }
    }

    /// Test helper: drive one worker cycle against the current (virtual) clock.
    #[cfg(test)]
    fn tick(&self) {
        let (tx, rx) = mpsc::channel();
        if self.send_control(Control::Tick(Completion::Blocking(tx))) {
            let _ = rx.recv();
        }
    }

    /// Test helper: blocking flush (the async client uses a oneshot instead).
    #[cfg(test)]
    fn flush_blocking(&self) {
        let (tx, rx) = mpsc::channel();
        if self.send_control(Control::Flush(Completion::Blocking(tx))) {
            let _ = rx.recv();
        }
    }

    /// Test helper: flush + stop + join, mirroring the client's shutdown.
    #[cfg(test)]
    fn shutdown_blocking(&self) {
        if !self.begin_close() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        if self.send_control(Control::Shutdown(Completion::Blocking(tx))) {
            let _ = rx.recv();
        }
        self.join();
    }
}

/// Reserve a queue slot under the bounded-capacity cap. Returns `false` (and
/// warns once) when full. A CAS keeps the count exact under concurrent producers.
fn try_reserve(len: &AtomicUsize, max: usize, warned: &AtomicBool) -> bool {
    loop {
        let current = len.load(Ordering::Acquire);
        if current >= max {
            if !warned.swap(true, Ordering::AcqRel) {
                warn!("posthog-rs: event queue full (capacity {max}); dropping events");
            }
            return false;
        }
        if len
            .compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            // Re-arm the full-queue warning once the queue has fully drained, so a
            // service that repeatedly fills then drains warns once per episode
            // instead of only on the very first overflow.
            if current == 0 {
                warned.store(false, Ordering::Release);
            }
            return true;
        }
    }
}

/// Decrement the in-flight counter by `n` (no-op for 0). Called when events reach
/// a terminal outcome (delivered or dropped) so `pending()` reflects everything
/// still in flight: channel + worker buffer + retry queue.
fn dec_len(len: &AtomicUsize, n: usize) {
    if n > 0 {
        len.fetch_sub(n, Ordering::AcqRel);
    }
}

/// Time until the next scheduled wakeup: the buffer's flush-interval deadline or
/// the earliest retry, whichever is sooner. `None` means nothing is pending, so
/// the worker should block on `recv` until a message arrives.
fn compute_wait(
    now: Instant,
    buffer_since: Option<Instant>,
    flush_interval: Duration,
    earliest_retry: Option<Instant>,
) -> Option<Duration> {
    let deadline = match (buffer_since, earliest_retry) {
        (Some(since), Some(retry)) => Some((since + flush_interval).min(retry)),
        (Some(since), None) => Some(since + flush_interval),
        (None, Some(retry)) => Some(retry),
        (None, None) => None,
    };
    deadline.map(|d| d.saturating_duration_since(now))
}

/// Number of concurrent sender threads. Telemetry sends are network-bound, so we
/// only need enough concurrency that one slow or stalled endpoint can't
/// head-of-line block the rest; capped so a many-core host doesn't oversubscribe.
fn sender_pool_size() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().min(4))
        .unwrap_or(2)
}

/// A built batch handed to a sender thread for one blocking HTTP attempt.
struct SendJob {
    batch: RetryBatch,
    final_attempt: bool,
}

/// What a sender reports back after one attempt. Terminal outcomes (delivered or
/// dropped) have already adjusted `len`; `Retry` carries the batch back so the
/// dispatcher can reschedule it at `batch.next_at`.
enum SendResult {
    Done,
    Retry(RetryBatch),
}

/// Sender thread body: pull built batches, POST them (blocking), report outcomes.
fn run_sender(
    pipeline: Arc<Pipeline>,
    jobs: Receiver<SendJob>,
    outcomes: Sender<SendResult>,
    abandon: Arc<AtomicBool>,
) {
    while let Ok(job) = jobs.recv() {
        // The shutdown deadline fired: drop jobs still queued for this sender
        // instead of POSTing them, so a closed client can't keep doing network
        // work past `shutdown_timeout`. (An already in-flight POST still finishes,
        // bounded by the request timeout — it can't be cancelled mid-flight.)
        if abandon.load(Ordering::Acquire) {
            pipeline.drop_batch(job.batch);
            continue;
        }
        let result = pipeline.attempt(job.batch, job.final_attempt);
        if outcomes.send(result).is_err() {
            return; // dispatcher gone
        }
    }
}

/// Owns the buffer, retry queue, and schedule on a single thread (so neither
/// needs a lock) and fans batch sends out to the pool. A send never blocks the
/// dispatcher; `flush`/`shutdown`/`Tick` are barriers that complete once the
/// sends they dispatched report back (`in_flight` returns to zero). While a
/// barrier is pending, no new threshold/interval/retry sends are started, so
/// `in_flight` can drain to zero.
struct Dispatcher {
    pipeline: Arc<Pipeline>,
    jobs: Sender<SendJob>,
    /// Shared with the senders: set when the shutdown deadline fires so they drop
    /// queued jobs instead of POSTing them after the dispatcher has exited.
    abandon: Arc<AtomicBool>,
    clock: Arc<dyn Clock>,
    flush_at: usize,
    max_batch_size: usize,
    flush_interval: Duration,
    shutdown_timeout: Duration,
    buffer: Vec<Event>,
    buffer_hist: bool,
    buffer_since: Option<Instant>,
    retries: VecDeque<RetryBatch>,
    /// Batches dispatched to the pool but not yet reported back.
    in_flight: usize,
    /// Flush/Tick completions awaiting `in_flight == 0`.
    flush_waiters: Vec<Completion>,
    /// Set once shutdown/disconnect begins; carries the drain deadline.
    shutdown_deadline: Option<Instant>,
    shutdown_completion: Option<Completion>,
    /// Control channel hung up (all clients dropped without an explicit shutdown).
    disconnected: bool,
    done: bool,
}

impl Dispatcher {
    #[allow(clippy::too_many_arguments)]
    fn new(
        pipeline: Arc<Pipeline>,
        jobs: Sender<SendJob>,
        abandon: Arc<AtomicBool>,
        clock: Arc<dyn Clock>,
        flush_at: usize,
        max_batch_size: usize,
        flush_interval: Duration,
        shutdown_timeout: Duration,
    ) -> Self {
        Self {
            pipeline,
            jobs,
            abandon,
            clock,
            flush_at,
            max_batch_size,
            flush_interval,
            shutdown_timeout,
            buffer: Vec::new(),
            buffer_hist: false,
            buffer_since: None,
            retries: VecDeque::new(),
            in_flight: 0,
            flush_waiters: Vec::new(),
            shutdown_deadline: None,
            shutdown_completion: None,
            disconnected: false,
            done: false,
        }
    }

    /// While draining, no new threshold/interval/retry sends are started so
    /// `in_flight` can fall to zero and the pending barrier(s) can complete.
    fn draining(&self) -> bool {
        !self.flush_waiters.is_empty() || self.shutdown_deadline.is_some() || self.disconnected
    }

    /// Time until the next wakeup. `None` means block on the channels (idle, or
    /// draining a flush — bounded by the per-attempt request timeout).
    fn wait(&self) -> Option<Duration> {
        if let Some(deadline) = self.shutdown_deadline {
            return Some(deadline.saturating_duration_since(self.clock.now()));
        }
        if !self.flush_waiters.is_empty() {
            return None;
        }
        compute_wait(
            self.clock.now(),
            self.buffer_since,
            self.flush_interval,
            self.retries.iter().map(|b| b.next_at).min(),
        )
    }

    fn dispatch(&mut self, batch: RetryBatch, final_attempt: bool) {
        match self.jobs.send(SendJob {
            batch,
            final_attempt,
        }) {
            Ok(()) => self.in_flight += 1,
            // Pool gone (only at teardown): account for the lost events.
            Err(crossbeam_channel::SendError(job)) => self.pipeline.drop_batch(job.batch),
        }
    }

    /// Drain the buffer into batches of at most `max_batch_size`, dispatching each.
    fn dispatch_buffer(&mut self, final_attempt: bool) {
        while !self.buffer.is_empty() {
            let take = self.buffer.len().min(self.max_batch_size);
            let chunk: Vec<Event> = self.buffer.drain(..take).collect();
            if let Some(batch) = self.pipeline.build(chunk, self.buffer_hist) {
                self.dispatch(batch, final_attempt);
            }
        }
        self.buffer_since = None;
    }

    fn dispatch_all_retries(&mut self, final_attempt: bool) {
        for batch in std::mem::take(&mut self.retries) {
            self.dispatch(batch, final_attempt);
        }
    }

    fn dispatch_due_retries(&mut self) {
        let now = self.clock.now();
        for batch in std::mem::take(&mut self.retries) {
            if now >= batch.next_at {
                self.dispatch(batch, false);
            } else {
                self.retries.push_back(batch);
            }
        }
    }

    fn drop_buffer(&mut self) {
        if !self.buffer.is_empty() {
            warn!(
                "posthog-rs: shutdown timeout reached; dropping {} buffered event(s)",
                self.buffer.len()
            );
            dec_len(&self.pipeline.len, self.buffer.len());
            self.buffer.clear();
        }
        self.buffer_since = None;
    }

    fn drop_all_retries(&mut self) {
        for batch in std::mem::take(&mut self.retries) {
            self.pipeline.drop_batch(batch);
        }
    }

    /// Register a flush/tick completion, or signal it now if nothing is in flight.
    fn register_barrier(&mut self, completion: Completion) {
        if self.in_flight == 0 {
            completion.signal();
        } else {
            self.flush_waiters.push(completion);
        }
    }

    fn on_control(&mut self, msg: Result<Control, crossbeam_channel::RecvError>) {
        let msg = match msg {
            Ok(m) => m,
            Err(_) => return self.on_disconnect(),
        };
        match msg {
            Control::Capture {
                event,
                historical_migration,
            } => {
                // `len` is not decremented here: the in-flight counter spans the
                // whole lifecycle (channel + buffer + sends + retries) and is
                // adjusted by the pipeline once a batch is delivered or dropped.
                // Keep each batch homogeneous in historical_migration: flush the
                // current buffer before mixing in an event with a different flag.
                if !self.buffer.is_empty() && historical_migration != self.buffer_hist {
                    self.dispatch_buffer(false);
                }
                if self.buffer.is_empty() {
                    self.buffer_hist = historical_migration;
                    self.buffer_since = Some(self.clock.now());
                }
                self.buffer.push(*event);
                if !self.draining() && self.buffer.len() >= self.flush_at {
                    self.dispatch_buffer(false);
                }
            }
            Control::Flush(completion) => {
                // One attempt per pending batch (held retries + buffered events);
                // failures are held for a future cycle. Completes once those sends
                // report back.
                self.dispatch_all_retries(false);
                self.dispatch_buffer(false);
                self.register_barrier(completion);
            }
            Control::Shutdown(completion) => {
                let deadline = self.clock.now() + self.shutdown_timeout;
                if self.clock.now() >= deadline {
                    // Zero/elapsed grace: drop without attempting.
                    self.drop_all_retries();
                    self.drop_buffer();
                } else {
                    self.dispatch_all_retries(true);
                    self.dispatch_buffer(true);
                }
                if self.in_flight == 0 {
                    completion.signal();
                    self.done = true;
                } else {
                    self.shutdown_deadline = Some(deadline);
                    self.shutdown_completion = Some(completion);
                }
            }
            #[cfg(test)]
            Control::Tick(completion) => {
                if !self.draining() {
                    if self
                        .buffer_since
                        .is_some_and(|s| self.clock.now().duration_since(s) >= self.flush_interval)
                    {
                        self.dispatch_buffer(false);
                    }
                    self.dispatch_due_retries();
                }
                self.register_barrier(completion);
            }
        }
    }

    fn on_outcome(&mut self, res: Result<SendResult, crossbeam_channel::RecvError>) {
        let Ok(result) = res else {
            return;
        };
        self.in_flight = self.in_flight.saturating_sub(1);
        if let SendResult::Retry(batch) = result {
            match self.shutdown_deadline {
                // Mid-teardown. A final attempt never returns Retry, so this batch
                // was dispatched before shutdown and failed transiently; it is owed
                // one final attempt. Give it one while the deadline allows (matching
                // the pre-pool single-worker behavior); past the deadline, drop it.
                Some(deadline) if self.clock.now() < deadline => self.dispatch(batch, true),
                Some(_) => self.pipeline.drop_batch(batch),
                None => self.retries.push_back(batch),
            }
        }
        if self.in_flight == 0 {
            for c in self.flush_waiters.drain(..) {
                c.signal();
            }
            if self.shutdown_deadline.is_some() {
                if let Some(c) = self.shutdown_completion.take() {
                    c.signal();
                }
                self.done = true;
            }
        }
    }

    fn on_timeout(&mut self) {
        if self
            .shutdown_deadline
            .is_some_and(|d| self.clock.now() >= d)
        {
            // Deadline hit with sends still outstanding: tell the senders to drop
            // their queued jobs (any in-flight POST still finishes, bounded by the
            // request timeout), signal the waiter, and exit.
            self.abandon.store(true, Ordering::Release);
            if let Some(c) = self.shutdown_completion.take() {
                c.signal();
            }
            self.done = true;
            return;
        }
        if !self.draining() {
            if self
                .buffer_since
                .is_some_and(|s| self.clock.now().duration_since(s) >= self.flush_interval)
            {
                self.dispatch_buffer(false);
            }
            self.dispatch_due_retries();
        }
    }

    fn on_disconnect(&mut self) {
        // All clients dropped without an explicit shutdown. Best-effort final
        // drain bounded by `shutdown_timeout`, then exit.
        self.disconnected = true;
        let deadline = self.clock.now() + self.shutdown_timeout;
        self.dispatch_all_retries(true);
        self.dispatch_buffer(true);
        if self.in_flight == 0 {
            self.done = true;
        } else {
            self.shutdown_deadline = Some(deadline);
        }
    }
}

fn run_worker(
    options: ClientOptions,
    control: Receiver<Control>,
    len: Arc<AtomicUsize>,
    clock: Arc<dyn Clock>,
) {
    let flush_at = options.flush_at.max(1);
    let max_batch_size = options.max_batch_size.max(1);
    let flush_interval = Duration::from_millis(options.flush_interval_ms);
    let shutdown_timeout = Duration::from_millis(options.shutdown_timeout_ms);
    let pipeline = Arc::new(Pipeline::new(&options, Arc::clone(&clock), len));

    // Sender pool: the blocking POST runs here, off the dispatcher, so a slow or
    // stalled endpoint can't head-of-line block draining. Senders are detached;
    // they exit when the dispatcher drops `job_tx` at teardown.
    let (job_tx, job_rx) = unbounded::<SendJob>();
    let (outcome_tx, outcome_rx) = unbounded::<SendResult>();
    let abandon = Arc::new(AtomicBool::new(false));
    for i in 0..sender_pool_size() {
        let pipeline = Arc::clone(&pipeline);
        let jobs = job_rx.clone();
        let outcomes = outcome_tx.clone();
        let abandon = Arc::clone(&abandon);
        let _ = thread::Builder::new()
            .name(format!("posthog-sender-{i}"))
            .spawn(move || run_sender(pipeline, jobs, outcomes, abandon));
    }
    drop(job_rx);
    drop(outcome_tx);

    let mut dispatcher = Dispatcher::new(
        pipeline,
        job_tx,
        Arc::clone(&abandon),
        clock,
        flush_at,
        max_batch_size,
        flush_interval,
        shutdown_timeout,
    );

    while !dispatcher.done {
        let wait = dispatcher.wait();
        if dispatcher.disconnected {
            // Control hung up; only sender outcomes remain to drain.
            match wait {
                Some(dur) => select! {
                    recv(outcome_rx) -> r => dispatcher.on_outcome(r),
                    default(dur) => dispatcher.on_timeout(),
                },
                None => match outcome_rx.recv() {
                    Ok(r) => dispatcher.on_outcome(Ok(r)),
                    Err(_) => dispatcher.done = true,
                },
            }
        } else {
            match wait {
                Some(dur) => select! {
                    recv(control) -> m => dispatcher.on_control(m),
                    recv(outcome_rx) -> r => dispatcher.on_outcome(r),
                    default(dur) => dispatcher.on_timeout(),
                },
                None => select! {
                    recv(control) -> m => dispatcher.on_control(m),
                    recv(outcome_rx) -> r => dispatcher.on_outcome(r),
                },
            }
        }
    }
    // `dispatcher` (and its `job_tx`) drops here, so the pool's senders see the
    // job channel close and exit after any in-flight POST completes.
}

// ===========================================================================
// V1 pipeline
// ===========================================================================

#[cfg(feature = "capture-v1")]
use std::collections::HashMap;
#[cfg(feature = "capture-v1")]
use uuid::Uuid;

#[cfg(feature = "capture-v1")]
struct RetryBatch {
    pending: Vec<crate::event_v1::V1Event>,
    request_id: Uuid,
    created_at: String,
    final_results: HashMap<Uuid, crate::event_v1::EventResult>,
    historical_migration: bool,
    attempt: u32,
    next_at: Instant,
}

#[cfg(feature = "capture-v1")]
struct Pipeline {
    http: reqwest::blocking::Client,
    options: ClientOptions,
    url: String,
    clock: Arc<dyn Clock>,
    len: Arc<AtomicUsize>,
}

#[cfg(feature = "capture-v1")]
impl Pipeline {
    fn new(options: &ClientOptions, clock: Arc<dyn Clock>, len: Arc<AtomicUsize>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(options.request_timeout_seconds))
            .build()
            .unwrap_or_default();
        let url = options
            .endpoints()
            .build_custom_url(super::v1_capture::V1_CAPTURE_PATH);
        Self {
            http,
            options: options.clone(),
            url,
            clock,
            len,
        }
    }

    /// Dispatcher-side: apply defaults + before_send and build the wire batch.
    /// Returns `None` when every event was dropped by before_send.
    fn build(&self, events: Vec<Event>, historical_migration: bool) -> Option<RetryBatch> {
        use super::common::{apply_before_send_hooks, apply_capture_defaults};

        let defaults = self.options.capture_defaults();
        let original = events.len();
        let processed: Vec<Event> = events
            .into_iter()
            .filter_map(|mut event| {
                apply_capture_defaults(&mut event, &defaults);
                apply_before_send_hooks(&self.options.before_send, event)
            })
            .collect();
        // Events dropped by before_send are terminal.
        dec_len(&self.len, original - processed.len());
        if processed.is_empty() {
            return None;
        }
        let pending =
            super::v1_capture::build_events_at(&processed, &defaults, self.clock.now_utc());
        Some(RetryBatch {
            pending,
            request_id: Uuid::now_v7(),
            created_at: self.clock.now_utc().to_rfc3339(),
            final_results: HashMap::new(),
            historical_migration,
            attempt: 1,
            next_at: self.clock.now(),
        })
    }

    /// Sender-side: one blocking HTTP attempt. Terminal outcomes adjust `len` and
    /// return `Done`; a transient failure returns `Retry` with `next_at` set.
    fn attempt(&self, mut batch: RetryBatch, final_attempt: bool) -> SendResult {
        use super::v1_capture::{self, Step};
        use crate::event_v1::V1BatchRequestRef;

        let req = V1BatchRequestRef {
            created_at: &batch.created_at,
            historical_migration: batch.historical_migration.then_some(true),
            batch: &batch.pending,
        };
        let payload = match serde_json::to_vec(&req) {
            Ok(p) => p,
            Err(e) => {
                warn!(
                    "posthog-rs: dropping {} event(s), serialization failed: {e}",
                    batch.pending.len()
                );
                dec_len(&self.len, batch.pending.len());
                return SendResult::Done;
            }
        };
        let mut headers = v1_capture::build_headers_at(
            &self.options,
            &batch.request_id,
            batch.attempt,
            self.clock.now_utc(),
        );
        let body =
            v1_capture::maybe_compress(self.options.capture_compression, &mut headers, payload);

        let count = batch.pending.len();
        let step = match self.http.post(&self.url).headers(headers).body(body).send() {
            Err(e) => v1_capture::after_transport_error(
                &self.options,
                &batch.request_id,
                batch.attempt,
                e.to_string(),
            ),
            Ok(resp) => {
                let status = resp.status().as_u16();
                let retry_after = v1_capture::parse_retry_after(resp.headers());
                let text = resp.text().unwrap_or_else(|_| "Unknown error".to_string());
                v1_capture::after_response(
                    &self.options,
                    &batch.request_id,
                    batch.attempt,
                    status,
                    retry_after,
                    &text,
                    &mut batch.pending,
                    &mut batch.final_results,
                )
            }
        };

        // Events that left `pending` (the ok/drop/warning subset) are terminal.
        dec_len(&self.len, count - batch.pending.len());

        match step {
            Step::Done => SendResult::Done,
            Step::Fail(e) => {
                warn!("posthog-rs: dropping {} event(s): {e}", batch.pending.len());
                dec_len(&self.len, batch.pending.len());
                SendResult::Done
            }
            Step::Backoff(delay) => {
                if final_attempt {
                    warn!(
                        "posthog-rs: dropping {} undelivered event(s) on shutdown",
                        batch.pending.len()
                    );
                    dec_len(&self.len, batch.pending.len());
                    SendResult::Done
                } else {
                    batch.attempt += 1;
                    batch.next_at = self.clock.now() + delay;
                    SendResult::Retry(batch)
                }
            }
        }
    }

    /// Account for a built-but-unsent batch (shutdown deadline / pool gone).
    fn drop_batch(&self, batch: RetryBatch) {
        dec_len(&self.len, batch.pending.len());
    }
}

// ===========================================================================
// V0 pipeline
// ===========================================================================

#[cfg(not(feature = "capture-v1"))]
struct RetryBatch {
    body: Vec<u8>,
    encoding: Option<&'static str>,
    count: usize,
    attempt: u32,
    next_at: Instant,
}

#[cfg(not(feature = "capture-v1"))]
struct Pipeline {
    http: reqwest::blocking::Client,
    options: ClientOptions,
    url_base: String,
    clock: Arc<dyn Clock>,
    len: Arc<AtomicUsize>,
}

#[cfg(not(feature = "capture-v1"))]
impl Pipeline {
    fn new(options: &ClientOptions, clock: Arc<dyn Clock>, len: Arc<AtomicUsize>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(options.request_timeout_seconds))
            .build()
            .unwrap_or_default();
        let url_base = options
            .endpoints()
            .build_url(crate::endpoints::Endpoint::Batch);
        Self {
            http,
            options: options.clone(),
            url_base,
            clock,
            len,
        }
    }

    /// Dispatcher-side: apply defaults + before_send and serialize the wire body.
    /// Returns `None` when every event was dropped by before_send.
    fn build(&self, events: Vec<Event>, historical_migration: bool) -> Option<RetryBatch> {
        let defaults = self.options.capture_defaults();
        let count = events.len();
        let payload = match super::v0_capture::build_batch_payload(
            events,
            self.options.api_key.clone(),
            historical_migration,
            self.clock.now_utc(),
            &defaults,
            &self.options.before_send,
        ) {
            Ok(Some(p)) => p,
            Ok(None) => {
                // Every event dropped by before_send (terminal).
                dec_len(&self.len, count);
                return None;
            }
            Err(e) => {
                warn!("posthog-rs: dropping {count} event(s), serialization failed: {e}");
                dec_len(&self.len, count);
                return None;
            }
        };
        let (body, encoding) = super::v0_capture::encode_body(&self.options, payload);
        Some(RetryBatch {
            body,
            encoding,
            count,
            attempt: 1,
            next_at: self.clock.now(),
        })
    }

    /// Sender-side: one blocking HTTP attempt. Terminal outcomes adjust `len` and
    /// return `Done`; a transient failure returns `Retry` with `next_at` set.
    fn attempt(&self, mut batch: RetryBatch, final_attempt: bool) -> SendResult {
        use super::get_default_user_agent;
        use super::retry::{v0_after_response, v0_after_transport_error, Step};
        use reqwest::header::{CONTENT_TYPE, USER_AGENT};

        // v0 capture reads the compression hint from the query param, not the header.
        let url = match batch.encoding {
            Some(token) => format!("{}?compression={token}", self.url_base),
            None => self.url_base.clone(),
        };
        let mut request = self
            .http
            .post(&url)
            .header(CONTENT_TYPE, "application/json")
            .header(USER_AGENT, get_default_user_agent())
            .body(batch.body.clone());
        if let Some(token) = batch.encoding {
            request = request.header(reqwest::header::CONTENT_ENCODING, token);
        }
        let request = super::v0_capture::apply_extra_headers(&self.options, request);

        let step = match request.send() {
            Err(e) => v0_after_transport_error(&self.options, batch.attempt, e.to_string()),
            Ok(response) => {
                let status = response.status().as_u16();
                let retry_after = super::retry::parse_retry_after(response.headers());
                let body = response
                    .text()
                    .unwrap_or_else(|_| "Unknown error".to_string());
                v0_after_response(&self.options, batch.attempt, status, retry_after, &body)
            }
        };

        match step {
            Step::Done => {
                dec_len(&self.len, batch.count);
                SendResult::Done
            }
            Step::Fail(e) => {
                warn!("posthog-rs: dropping {} event(s): {e}", batch.count);
                dec_len(&self.len, batch.count);
                SendResult::Done
            }
            Step::Backoff(delay) => {
                if final_attempt {
                    warn!(
                        "posthog-rs: dropping {} undelivered event(s) on shutdown",
                        batch.count
                    );
                    dec_len(&self.len, batch.count);
                    SendResult::Done
                } else {
                    batch.attempt += 1;
                    batch.next_at = self.clock.now() + delay;
                    SendResult::Retry(batch)
                }
            }
        }
    }

    /// Account for a built-but-unsent batch (shutdown deadline / pool gone).
    fn drop_batch(&self, batch: RetryBatch) {
        dec_len(&self.len, batch.count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ClientOptionsBuilder;
    use httpmock::prelude::*;

    /// Test clock with manually advanced virtual time, so interval and backoff
    /// timing are exercised without real sleeps.
    #[derive(Clone)]
    struct ManualClock {
        inner: Arc<Mutex<(Instant, DateTime<Utc>)>>,
    }

    impl ManualClock {
        fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new((Instant::now(), Utc::now()))),
            }
        }
        fn advance(&self, by: Duration) {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.0 += by;
            g.1 += chrono::Duration::from_std(by).expect("test duration fits chrono");
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            self.inner.lock().unwrap_or_else(|p| p.into_inner()).0
        }
        fn now_utc(&self) -> DateTime<Utc> {
            self.inner.lock().unwrap_or_else(|p| p.into_inner()).1
        }
    }

    fn ok_mock(server: &MockServer) -> httpmock::Mock<'_> {
        server.mock(|when, then| {
            when.method(POST);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({ "results": {} }));
        })
    }

    fn options(base_url: String) -> ClientOptionsBuilder {
        let mut builder = ClientOptionsBuilder::default();
        builder
            .api_key("phc_test".to_string())
            .host(base_url)
            .flush_at(100usize)
            .flush_interval_ms(10_000u64);
        builder
    }

    // -- pure helpers --------------------------------------------------------

    #[test]
    fn try_reserve_bounds_at_capacity_and_warns_once() {
        let len = AtomicUsize::new(0);
        let warned = AtomicBool::new(false);
        assert!(try_reserve(&len, 2, &warned));
        assert!(try_reserve(&len, 2, &warned));
        assert!(!try_reserve(&len, 2, &warned)); // third dropped
        assert_eq!(len.load(Ordering::Acquire), 2);
        assert!(warned.load(Ordering::Acquire));
        // A second overflow does not re-warn (single warning when full).
        assert!(!try_reserve(&len, 2, &warned));
    }

    #[test]
    fn try_reserve_rearms_warning_after_full_drain() {
        let len = AtomicUsize::new(0);
        let warned = AtomicBool::new(false);
        assert!(try_reserve(&len, 1, &warned));
        assert!(!try_reserve(&len, 1, &warned)); // full -> warns
        assert!(warned.load(Ordering::Acquire));
        len.fetch_sub(1, Ordering::AcqRel); // queue fully drains
        assert!(try_reserve(&len, 1, &warned)); // reserve from empty re-arms the warning
        assert!(!warned.load(Ordering::Acquire));
        // A fresh overflow warns again (a new full episode).
        assert!(!try_reserve(&len, 1, &warned));
        assert!(warned.load(Ordering::Acquire));
    }

    #[test]
    fn compute_wait_picks_interval_then_zero_when_elapsed() {
        let base = Instant::now();
        let interval = Duration::from_secs(10);
        assert_eq!(
            compute_wait(base, Some(base), interval, None),
            Some(interval)
        );
        assert_eq!(
            compute_wait(base + Duration::from_secs(10), Some(base), interval, None),
            Some(Duration::ZERO)
        );
        assert_eq!(
            compute_wait(base + Duration::from_secs(9), Some(base), interval, None),
            Some(Duration::from_secs(1))
        );
    }

    #[test]
    fn compute_wait_blocks_when_idle_and_prefers_earliest() {
        let base = Instant::now();
        let interval = Duration::from_secs(10);
        assert_eq!(compute_wait(base, None, interval, None), None);
        let retry_at = base + Duration::from_secs(2);
        assert_eq!(
            compute_wait(base, Some(base), interval, Some(retry_at)),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            compute_wait(base, None, interval, Some(retry_at)),
            Some(Duration::from_secs(2))
        );
    }

    // -- virtual-clock worker tests (no real sleeps) -------------------------

    #[test]
    fn interval_flush_fires_on_clock_advance() {
        let server = MockServer::start();
        let mock = ok_mock(&server);
        let clock = ManualClock::new();
        let handle = TransportHandle::spawn_with_clock(
            options(server.base_url()).build().unwrap(),
            Arc::new(clock.clone()),
        );

        handle.enqueue(Event::new("Delayed", "user-1"), false);
        handle.tick(); // interval not yet elapsed
        mock.assert_hits(0);
        assert_eq!(
            handle.pending(),
            1,
            "buffered-but-undelivered event stays in flight"
        );

        clock.advance(Duration::from_secs(10));
        handle.tick(); // interval elapsed -> flush, no real sleep
        mock.assert_hits(1);
        assert_eq!(
            handle.pending(),
            0,
            "delivered event is decremented from in flight"
        );

        handle.shutdown_blocking();
    }

    #[test]
    fn retry_backoff_is_honored_against_the_clock() {
        let server = MockServer::start();
        let mut fail = server.mock(|when, then| {
            when.method(POST);
            then.status(503);
        });
        let clock = ManualClock::new();
        let handle = TransportHandle::spawn_with_clock(
            options(server.base_url())
                .max_capture_attempts(5u32)
                .retry_initial_backoff_ms(1_000u64)
                .retry_max_backoff_ms(60_000u64)
                .build()
                .unwrap(),
            Arc::new(clock.clone()),
        );

        handle.enqueue(Event::new("Save", "user-1"), false);
        handle.flush_blocking(); // attempt 1 -> 503, held with next_at = now + 1s
        fail.assert_hits(1);

        handle.tick(); // backoff not elapsed -> no retry
        fail.assert_hits(1);

        fail.delete();
        let ok = ok_mock(&server);
        clock.advance(Duration::from_secs(1)); // now >= next_at
        handle.tick(); // due -> retried, delivered
        ok.assert_hits(1);

        handle.shutdown_blocking();
    }

    /// Parse either an RFC3339 string (v1 timestamps / v0 `sent_at`) or a naive
    /// datetime (v0 event timestamps serialize without an offset) as UTC.
    fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(s)
            .map(|d| d.with_timezone(&Utc))
            .ok()
            .or_else(|| {
                chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
                    .map(|n| n.and_utc())
                    .ok()
            })
    }

    #[test]
    fn event_timestamp_is_capture_time_not_publish_time() {
        // An event captured at T0 but flushed 10s later must carry T0 as its
        // event `timestamp` (when it occurred), while the batch envelope carries
        // the publish time (v1 `created_at` / v0 `sent_at`). The check is encoded
        // in the matcher: the request only matches when publish - timestamp ~= 10s,
        // proving the stamp happens at enqueue, not at send. Holds for both wire
        // shapes (both nest the event under `batch[0]`).
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).matches(|req| {
                let Some(bytes) = req.body.as_deref() else {
                    return false;
                };
                let Ok(json) = serde_json::from_slice::<serde_json::Value>(bytes) else {
                    return false;
                };
                let event_ts = json["batch"][0]["timestamp"].as_str().and_then(parse_ts);
                let publish_ts = json
                    .get("created_at")
                    .or_else(|| json.get("sent_at"))
                    .and_then(|v| v.as_str())
                    .and_then(parse_ts);
                match (event_ts, publish_ts) {
                    (Some(e), Some(p)) => (9_900..=10_100).contains(&(p - e).num_milliseconds()),
                    _ => false,
                }
            });
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({ "results": {} }));
        });

        let clock = ManualClock::new();
        let handle = TransportHandle::spawn_with_clock(
            options(server.base_url()).build().unwrap(),
            Arc::new(clock.clone()),
        );

        handle.enqueue(Event::new("Captured", "user-1"), false); // stamped at T0
        clock.advance(Duration::from_secs(10)); // ...delivered 10s later
        handle.flush_blocking();

        mock.assert_hits(1);
        handle.shutdown_blocking();
    }

    #[test]
    fn shutdown_timeout_drops_undelivered_without_blocking() {
        // shutdown_timeout = 0: the drain deadline is already past when the worker
        // handles Shutdown, so a buffered event is dropped (not sent) and teardown
        // returns instead of blocking on the endpoint.
        let server = MockServer::start();
        let mock = ok_mock(&server);
        let clock = ManualClock::new();
        let handle = TransportHandle::spawn_with_clock(
            options(server.base_url())
                .shutdown_timeout_ms(0u64)
                .build()
                .unwrap(),
            Arc::new(clock.clone()),
        );

        handle.enqueue(Event::new("Dropped", "user-1"), false);
        handle.shutdown_blocking(); // deadline already past -> drop, do not send

        mock.assert_hits(0);
        assert_eq!(
            handle.pending(),
            0,
            "dropped events leave nothing in flight"
        );
    }

    #[test]
    fn flush_waits_for_every_batch_across_the_pool() {
        // max_batch_size = 1 forces one batch per event, so a single flush fans
        // several sends out to the pool concurrently. The flush barrier must wait
        // for *all* of them to report back before returning.
        let server = MockServer::start();
        let mock = ok_mock(&server);
        let clock = ManualClock::new();
        let handle = TransportHandle::spawn_with_clock(
            options(server.base_url())
                .max_batch_size(1usize)
                .build()
                .unwrap(),
            Arc::new(clock.clone()),
        );

        for _ in 0..5 {
            handle.enqueue(Event::new("E", "user-1"), false);
        }
        handle.flush_blocking();

        mock.assert_hits(5);
        assert_eq!(handle.pending(), 0, "all batches delivered after flush");
        handle.shutdown_blocking();
    }

    #[test]
    fn sender_abandons_queued_jobs_without_posting() {
        // With the abandon flag set (shutdown deadline passed), a sender drops
        // queued jobs and accounts for them instead of POSTing, so a closed client
        // stops doing network work even with batches still queued.
        let server = MockServer::start();
        let mock = ok_mock(&server);
        let len = Arc::new(AtomicUsize::new(1));
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new());
        let pipeline = Arc::new(Pipeline::new(
            &options(server.base_url()).build().unwrap(),
            Arc::clone(&clock),
            Arc::clone(&len),
        ));
        let batch = pipeline
            .build(vec![Event::new("E", "user-1")], false)
            .expect("batch built");

        let (job_tx, job_rx) = unbounded::<SendJob>();
        let (out_tx, _out_rx) = unbounded::<SendResult>();
        job_tx
            .send(SendJob {
                batch,
                final_attempt: true,
            })
            .unwrap();
        drop(job_tx); // sender's recv ends once the queue drains

        run_sender(pipeline, job_rx, out_tx, Arc::new(AtomicBool::new(true)));

        mock.assert_hits(0);
        assert_eq!(
            len.load(Ordering::Acquire),
            0,
            "abandoned batch is dropped from in-flight, not sent"
        );
    }
}

//! Runtime-independent event transport.
//!
//! A single background `std::thread` drains a channel, batches events, sends
//! them with **blocking** reqwest, and retries transient failures on a schedule.
//! Being a plain thread with a blocking client (never a tokio task) it works for
//! the async client, the blocking client, and — in a later change — a
//! `std::panic` hook with no runtime present.
//!
//! `capture()` becomes a non-blocking enqueue (`Control::Capture`). `flush()` and
//! `shutdown()` send a control message carrying a [`Completion`] the worker
//! signals once the requested work is done, bridging the std-thread worker to
//! either an async (`oneshot`) or blocking (`mpsc`) caller without putting a
//! runtime in the worker.
//!
//! A [`Clock`] is injected into the worker so the interval timer, retry backoff,
//! and v1 wire timestamps are deterministic in tests (a `ManualClock` plus a
//! test-only `Tick` command drive the worker with virtual time — no real sleeps).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tracing::warn;

use super::ClientOptions;
use crate::Event;

/// Messages sent from producers (`capture`/`flush`/`shutdown`) to the worker.
pub(crate) enum Control {
    // Boxed so the channel node isn't sized to a whole Event on every flush/
    // shutdown message (clippy::large_enum_variant).
    Capture {
        event: Box<Event>,
    },
    /// A caller-formed historical-migration batch, sent as-is (chunked) on its
    /// own path so the live `Capture` buffer stays non-historical.
    HistoricalBatch {
        events: Vec<Event>,
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
    tx: mpsc::Sender<Control>,
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
        let (tx, rx) = mpsc::channel::<Control>();
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
    pub(crate) fn enqueue(&self, mut event: Event) {
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
            })
            .is_err()
        {
            // Worker gone; release the slot we reserved.
            self.len.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Enqueue a caller-formed historical-migration batch on its own path, kept
    /// off the live buffer (which is always non-historical). Reserves a queue
    /// slot per event up to the bound, dropping any overflow with the usual
    /// once-per-episode full warning.
    pub(crate) fn enqueue_historical(&self, mut events: Vec<Event>) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let mut fitted = 0;
        while fitted < events.len()
            && try_reserve(&self.len, self.max_queue_size, &self.full_warned)
        {
            events[fitted].ensure_timestamp(self.clock.now_utc());
            fitted += 1;
        }
        events.truncate(fitted);
        if events.is_empty() {
            return;
        }
        if self.tx.send(Control::HistoricalBatch { events }).is_err() {
            self.len.fetch_sub(fitted, Ordering::AcqRel);
        }
    }

    /// Send a flush/shutdown control message. Returns `false` once the worker has
    /// exited (the channel is disconnected), so a caller's wait is skipped rather
    /// than hanging. A control that races in just before the worker exits is still
    /// unblocked: the worker signals queued completions on the way out (see
    /// `drain_pending_completions`), and any it doesn't reach are dropped with the
    /// channel — which wakes the caller's wait with a recv error.
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

/// Per-request timeout for a send. On the shutdown/disconnect path (`deadline`
/// is `Some`) the request is capped at the time left before the deadline — never
/// more than the configured request timeout — so a stalled endpoint that accepts
/// but never responds can't push teardown past `shutdown_timeout_ms`. Off that
/// path it keeps the full configured timeout.
fn bound_request(
    request: reqwest::blocking::RequestBuilder,
    deadline: Option<Instant>,
    now: Instant,
    request_timeout_seconds: u64,
) -> reqwest::blocking::RequestBuilder {
    match deadline {
        Some(d) => request.timeout(
            d.saturating_duration_since(now)
                .min(Duration::from_secs(request_timeout_seconds)),
        ),
        None => request,
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

enum Wake {
    Msg(Control),
    Timeout,
    Disconnected,
}

fn run_worker(
    options: ClientOptions,
    rx: mpsc::Receiver<Control>,
    len: Arc<AtomicUsize>,
    clock: Arc<dyn Clock>,
) {
    let flush_at = options.flush_at.max(1);
    let max_batch_size = options.max_batch_size.max(1);
    let flush_interval = Duration::from_millis(options.flush_interval_ms);
    // Clamp so an absurd `shutdown_timeout_ms` can't overflow the `now +
    // shutdown_timeout` deadlines below and panic the worker — the same class of
    // guard `RETRY_BACKOFF_CAP` applies to retry backoff. A day is far beyond any
    // sane teardown budget.
    let shutdown_timeout =
        Duration::from_millis(options.shutdown_timeout_ms).min(Duration::from_secs(86_400));
    let mut pipeline = Pipeline::new(&options, Arc::clone(&clock), len);

    let mut buffer: Vec<Event> = Vec::new();
    let mut buffer_since: Option<Instant> = None;
    // Caller-formed historical batches awaiting their own (chunked) send. Queued
    // rather than sent inline, and timed on the flush interval like the live
    // buffer, so the worker is blocked on `recv` when a Shutdown arrives and can
    // bound/abandon them under the deadline instead of racing the send.
    let mut historical: VecDeque<Vec<Event>> = VecDeque::new();
    let mut historical_since: Option<Instant> = None;

    loop {
        #[cfg(test)]
        let mut tick_completion: Option<Completion> = None;
        let wait = {
            let base = compute_wait(
                clock.now(),
                buffer_since,
                flush_interval,
                pipeline.earliest_retry(),
            );
            match historical_since {
                Some(since) => {
                    let hwait = (since + flush_interval).saturating_duration_since(clock.now());
                    Some(base.map_or(hwait, |w| w.min(hwait)))
                }
                None => base,
            }
        };
        let wake = match wait {
            None => match rx.recv() {
                Ok(msg) => Wake::Msg(msg),
                Err(_) => Wake::Disconnected,
            },
            Some(timeout) => match rx.recv_timeout(timeout) {
                Ok(msg) => Wake::Msg(msg),
                Err(mpsc::RecvTimeoutError::Timeout) => Wake::Timeout,
                Err(mpsc::RecvTimeoutError::Disconnected) => Wake::Disconnected,
            },
        };

        match wake {
            Wake::Msg(Control::Capture { event }) => {
                // `len` is not decremented here: the in-flight counter spans the
                // whole worker lifecycle (channel + buffer + retries) and is
                // decremented by the pipeline once a batch is delivered or dropped.
                if buffer.is_empty() {
                    buffer_since = Some(clock.now());
                }
                buffer.push(*event);
                if buffer.len() >= flush_at {
                    send_buffer(&mut pipeline, &mut buffer, max_batch_size, None);
                    buffer_since = None;
                }
            }
            Wake::Msg(Control::HistoricalBatch { mut events }) => {
                // Queue the chunks off the live buffer (which stays non-historical
                // — no per-event flag, no homogeneity flush). Below `flush_at` they
                // wait rather than being sent inline, so a Shutdown queued behind
                // this batch is observed first and bounds/abandons them under
                // `shutdown_timeout` like buffered events; once `flush_at` have
                // queued they're sent right away, mirroring the live buffer's size
                // threshold. Anything left is flushed on the interval.
                while !events.is_empty() {
                    let take = events.len().min(max_batch_size);
                    historical.push_back(events.drain(..take).collect());
                }
                if historical_since.is_none() {
                    historical_since = Some(clock.now());
                }
                if historical.iter().map(Vec::len).sum::<usize>() >= flush_at {
                    drain_historical(&mut pipeline, &mut historical, None);
                    historical_since = None;
                }
            }
            Wake::Msg(Control::Flush(completion)) => {
                // One delivery attempt per pending batch: retry the already-held
                // batches first, then queued historical batches and the freshly
                // buffered ones. Failures are held for the next cycle (so a single
                // 503 leaves the event queued rather than re-attempted right away).
                pipeline.flush_retries(None);
                drain_historical(&mut pipeline, &mut historical, None);
                historical_since = None;
                send_buffer(&mut pipeline, &mut buffer, max_batch_size, None);
                buffer_since = None;
                completion.signal();
            }
            Wake::Msg(Control::Shutdown(completion)) => {
                // Drain held retries, queued historical batches, then buffered
                // events — one final attempt each, bounded by `shutdown_timeout`:
                // once the deadline passes the rest is dropped so the drain can't
                // hang on a slow endpoint. (An automatic flush/drain in progress
                // when this Shutdown arrives runs to completion first — up to
                // `request_timeout_seconds` per in-flight batch — since the single
                // worker can't preempt it; see `shutdown_timeout_ms`.)
                let deadline = clock.now() + shutdown_timeout;
                pipeline.flush_retries(Some(deadline));
                drain_historical(&mut pipeline, &mut historical, Some(deadline));
                send_buffer(&mut pipeline, &mut buffer, max_batch_size, Some(deadline));
                completion.signal();
                // A flush/shutdown that raced in behind this Shutdown is still queued;
                // signal those completions so their callers don't block forever.
                drain_pending_completions(&rx, &pipeline.len);
                return;
            }
            #[cfg(test)]
            Wake::Msg(Control::Tick(completion)) => {
                // Defer the signal until after the shared servicing block below,
                // so a test sees the tick's interval/retry effects on return.
                tick_completion = Some(completion);
            }
            // Nothing arm-specific: the shared servicing block after the match
            // handles the interval flush and due retries for an idle timeout too.
            Wake::Timeout => {}
            Wake::Disconnected => {
                // All client handles dropped without an explicit shutdown — best
                // effort drain bounded by `shutdown_timeout`, then exit.
                let deadline = clock.now() + shutdown_timeout;
                drain_historical(&mut pipeline, &mut historical, Some(deadline));
                send_buffer(&mut pipeline, &mut buffer, max_batch_size, Some(deadline));
                pipeline.flush_retries(Some(deadline));
                return;
            }
        }

        // Service due timers after every wake, not only on the idle timeout:
        // under sustained capture traffic `recv` keeps returning a message and
        // never times out, so the interval flush and scheduled retries would
        // otherwise be postponed until producers pause. (Shutdown/Disconnected
        // return above, keeping their drain deadline-bounded.)
        if buffer_since.is_some_and(|since| clock.now().duration_since(since) >= flush_interval) {
            send_buffer(&mut pipeline, &mut buffer, max_batch_size, None);
            buffer_since = None;
        }
        if historical_since.is_some_and(|since| clock.now().duration_since(since) >= flush_interval)
        {
            drain_historical(&mut pipeline, &mut historical, None);
            historical_since = None;
        }
        pipeline.attempt_due();
        #[cfg(test)]
        if let Some(completion) = tick_completion {
            completion.signal();
        }
    }
}

/// Signal any flush/shutdown completions still queued when the worker exits, so a
/// caller whose control message raced in behind the `Shutdown` doesn't block forever
/// on a completion that will never be processed. Queued captures are dropped, but
/// their reserved in-flight slots are released so `pending_events()` settles to 0.
fn drain_pending_completions(rx: &mpsc::Receiver<Control>, len: &AtomicUsize) {
    while let Ok(control) = rx.try_recv() {
        match control {
            Control::Flush(c) | Control::Shutdown(c) => c.signal(),
            #[cfg(test)]
            Control::Tick(c) => c.signal(),
            Control::Capture { .. } => dec_len(len, 1),
            Control::HistoricalBatch { events } => dec_len(len, events.len()),
        }
    }
}

/// Drain `buffer` into batches of at most `max_batch_size`, FIFO from the front,
/// attempting each once. `deadline` is `Some` only on the shutdown/disconnect
/// path: those attempts are final (warn-and-drop on transient failure instead of
/// scheduling a retry), and once the deadline passes the rest of the buffer is
/// dropped so teardown can't hang on a slow endpoint.
fn send_buffer(
    pipeline: &mut Pipeline,
    buffer: &mut Vec<Event>,
    max_batch_size: usize,
    deadline: Option<Instant>,
) {
    while !buffer.is_empty() {
        if deadline.is_some_and(|d| pipeline.clock.now() >= d) {
            warn!(
                "posthog-rs: shutdown timeout reached; dropping {} buffered event(s)",
                buffer.len()
            );
            dec_len(&pipeline.len, buffer.len());
            buffer.clear();
            return;
        }
        let take = buffer.len().min(max_batch_size);
        let chunk: Vec<Event> = buffer.drain(..take).collect();
        // The buffer only ever holds live events; historical batches take their
        // own path, so this is always a non-historical send.
        pipeline.send_batch(chunk, false, deadline);
    }
}

/// Drain queued historical-migration batches, FIFO. `deadline` is `Some` only on
/// the shutdown/disconnect path: those attempts are final, and once the deadline
/// passes the remaining queued batches are dropped so teardown can't hang.
fn drain_historical(
    pipeline: &mut Pipeline,
    historical: &mut VecDeque<Vec<Event>>,
    deadline: Option<Instant>,
) {
    while let Some(chunk) = historical.pop_front() {
        if deadline.is_some_and(|d| pipeline.clock.now() >= d) {
            let dropped = chunk.len() + historical.iter().map(Vec::len).sum::<usize>();
            warn!("posthog-rs: shutdown timeout reached; dropping {dropped} historical event(s)");
            dec_len(&pipeline.len, chunk.len());
            for rest in historical.drain(..) {
                dec_len(&pipeline.len, rest.len());
            }
            return;
        }
        pipeline.send_batch(chunk, true, deadline);
    }
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
    retries: VecDeque<RetryBatch>,
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
            retries: VecDeque::new(),
        }
    }

    fn send_batch(
        &mut self,
        events: Vec<Event>,
        historical_migration: bool,
        deadline: Option<Instant>,
    ) {
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
            return;
        }
        let now = self.clock.now();
        let pending =
            super::v1_capture::build_events_at(&processed, &defaults, self.clock.now_utc());
        let batch = RetryBatch {
            pending,
            request_id: Uuid::now_v7(),
            created_at: self.clock.now_utc().to_rfc3339(),
            final_results: HashMap::new(),
            historical_migration,
            attempt: 1,
            next_at: now,
        };
        self.attempt(batch, deadline);
    }

    fn attempt(&mut self, mut batch: RetryBatch, deadline: Option<Instant>) {
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
                return;
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
        let request = bound_request(
            self.http.post(&self.url).headers(headers).body(body),
            deadline,
            self.clock.now(),
            self.options.request_timeout_seconds,
        );
        let step = match request.send() {
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
            Step::Done => {}
            Step::Fail(e) => {
                warn!("posthog-rs: dropping {} event(s): {e}", batch.pending.len());
                dec_len(&self.len, batch.pending.len());
            }
            Step::Backoff(delay) => {
                if deadline.is_some() {
                    warn!(
                        "posthog-rs: dropping {} undelivered event(s) on shutdown",
                        batch.pending.len()
                    );
                    dec_len(&self.len, batch.pending.len());
                } else {
                    batch.attempt += 1;
                    batch.next_at = self.clock.now() + delay;
                    self.retries.push_back(batch);
                }
            }
        }
    }

    fn earliest_retry(&self) -> Option<Instant> {
        self.retries.iter().map(|b| b.next_at).min()
    }

    fn attempt_due(&mut self) {
        let now = self.clock.now();
        for batch in std::mem::take(&mut self.retries) {
            if now >= batch.next_at {
                self.attempt(batch, None);
            } else {
                self.retries.push_back(batch);
            }
        }
    }

    fn flush_retries(&mut self, deadline: Option<Instant>) {
        // `Some` is the shutdown/disconnect path: attempts are final (drop on
        // failure), and any batch still pending once the deadline passes is
        // dropped rather than attempted.
        for batch in std::mem::take(&mut self.retries) {
            if deadline.is_some_and(|d| self.clock.now() >= d) {
                warn!(
                    "posthog-rs: shutdown timeout reached; dropping {} undelivered event(s)",
                    batch.pending.len()
                );
                dec_len(&self.len, batch.pending.len());
            } else {
                self.attempt(batch, deadline);
            }
        }
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
    retries: VecDeque<RetryBatch>,
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
            retries: VecDeque::new(),
        }
    }

    fn send_batch(
        &mut self,
        events: Vec<Event>,
        historical_migration: bool,
        deadline: Option<Instant>,
    ) {
        let defaults = self.options.capture_defaults();
        let count = events.len();
        let (payload, kept) = match super::v0_capture::build_batch_payload(
            events,
            self.options.api_key.clone(),
            historical_migration,
            self.clock.now_utc(),
            &defaults,
            &self.options.before_send,
        ) {
            Ok(Some(pair)) => pair,
            Ok(None) => {
                // Every event dropped by before_send (terminal).
                dec_len(&self.len, count);
                return;
            }
            Err(e) => {
                warn!("posthog-rs: dropping {count} event(s), serialization failed: {e}");
                dec_len(&self.len, count);
                return;
            }
        };
        // Events dropped by before_send are terminal; account for them now so the
        // batch tracks (and logs) only what is actually in flight.
        dec_len(&self.len, count - kept);
        let (body, encoding) = super::v0_capture::encode_body(&self.options, payload);
        let batch = RetryBatch {
            body,
            encoding,
            count: kept,
            attempt: 1,
            next_at: self.clock.now(),
        };
        self.attempt(batch, deadline);
    }

    fn attempt(&mut self, mut batch: RetryBatch, deadline: Option<Instant>) {
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
        let request = bound_request(
            request,
            deadline,
            self.clock.now(),
            self.options.request_timeout_seconds,
        );

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
            Step::Done => dec_len(&self.len, batch.count),
            Step::Fail(e) => {
                warn!("posthog-rs: dropping {} event(s): {e}", batch.count);
                dec_len(&self.len, batch.count);
            }
            Step::Backoff(delay) => {
                if deadline.is_some() {
                    warn!(
                        "posthog-rs: dropping {} undelivered event(s) on shutdown",
                        batch.count
                    );
                    dec_len(&self.len, batch.count);
                } else {
                    batch.attempt += 1;
                    batch.next_at = self.clock.now() + delay;
                    self.retries.push_back(batch);
                }
            }
        }
    }

    fn earliest_retry(&self) -> Option<Instant> {
        self.retries.iter().map(|b| b.next_at).min()
    }

    fn attempt_due(&mut self) {
        let now = self.clock.now();
        for batch in std::mem::take(&mut self.retries) {
            if now >= batch.next_at {
                self.attempt(batch, None);
            } else {
                self.retries.push_back(batch);
            }
        }
    }

    fn flush_retries(&mut self, deadline: Option<Instant>) {
        // `Some` is the shutdown/disconnect path: attempts are final (drop on
        // failure), and any batch still pending once the deadline passes is
        // dropped rather than attempted.
        for batch in std::mem::take(&mut self.retries) {
            if deadline.is_some_and(|d| self.clock.now() >= d) {
                warn!(
                    "posthog-rs: shutdown timeout reached; dropping {} undelivered event(s)",
                    batch.count
                );
                dec_len(&self.len, batch.count);
            } else {
                self.attempt(batch, deadline);
            }
        }
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

    #[test]
    fn drain_pending_completions_signals_and_releases_queued_controls() {
        // Flush/Shutdown completions still queued when the worker exits must be
        // signaled so their callers don't hang; queued captures/batches are dropped
        // but must release their reserved in-flight slots.
        let (tx, rx) = mpsc::channel::<Control>();
        let (ftx, frx) = mpsc::channel::<()>();
        let (stx, srx) = mpsc::channel::<()>();
        let len = AtomicUsize::new(3); // 1 capture + 2 historical events reserved
        tx.send(Control::Flush(Completion::Blocking(ftx))).unwrap();
        tx.send(Control::Shutdown(Completion::Blocking(stx)))
            .unwrap();
        tx.send(Control::Capture {
            event: Box::new(Event::new("dropped", "user-1")),
        })
        .unwrap();
        tx.send(Control::HistoricalBatch {
            events: vec![Event::new("h1", "user-1"), Event::new("h2", "user-1")],
        })
        .unwrap();
        drop(tx);

        drain_pending_completions(&rx, &len);

        assert!(frx.recv().is_ok(), "flush completion was not signaled");
        assert!(srx.recv().is_ok(), "shutdown completion was not signaled");
        assert_eq!(
            len.load(Ordering::Acquire),
            0,
            "dropped events left counted as pending"
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

        handle.enqueue(Event::new("Delayed", "user-1"));
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

        handle.enqueue(Event::new("Save", "user-1"));
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

    #[test]
    fn before_send_dropped_events_are_not_counted_in_flight() {
        // before_send drops one of two events; a 503 holds the batch for retry.
        // pending() must reflect only the surviving event: the dropped one is
        // terminal at build time, so counting it as in-flight would inflate the
        // bounded-queue depth (and the drop/retry logs) for the batch's lifetime.
        let server = MockServer::start();
        let fail = server.mock(|when, then| {
            when.method(POST);
            then.status(503);
        });
        let clock = ManualClock::new();
        let handle = TransportHandle::spawn_with_clock(
            options(server.base_url())
                .before_send(|event| {
                    if event.properties().get("__drop").is_some() {
                        None
                    } else {
                        Some(event)
                    }
                })
                .build()
                .unwrap(),
            Arc::new(clock.clone()),
        );

        handle.enqueue(Event::new("keep", "user-1"));
        let mut dropped = Event::new("drop", "user-1");
        dropped.insert_prop("__drop", true).unwrap();
        handle.enqueue(dropped);
        assert_eq!(handle.pending(), 2); // both reserved in the bounded queue

        handle.flush_blocking(); // 1 kept + 1 filtered by before_send; 503 holds the kept one

        fail.assert_hits(1);
        // Only the surviving event remains in flight; the filtered one is terminal.
        assert_eq!(handle.pending(), 1);

        handle.shutdown_blocking();
    }

    #[test]
    fn shutdown_timeout_bounds_a_stalled_in_flight_send() {
        // Endpoint accepts then stalls far past shutdown_timeout_ms. The per-request
        // timeout must cap the in-flight send at the remaining deadline so teardown
        // returns near shutdown_timeout_ms rather than blocking for the full
        // request_timeout_seconds. Real time on purpose: this drives the reqwest
        // timeout, which the virtual ManualClock cannot.
        let server = MockServer::start();
        let _stall = server.mock(|when, then| {
            when.method(POST);
            then.status(200).delay(Duration::from_secs(5)).body("{}");
        });
        // Real system clock (spawn, not spawn_with_clock) so the deadline and the
        // reqwest timeout share the same wall clock.
        let handle = TransportHandle::spawn(
            options(server.base_url())
                .shutdown_timeout_ms(200u64)
                .build()
                .unwrap(),
        );
        handle.enqueue(Event::new("e", "user-1"));

        let start = Instant::now();
        handle.shutdown_blocking();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "shutdown blocked for {:?}; in-flight send was not bounded by shutdown_timeout_ms",
            elapsed
        );
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

        handle.enqueue(Event::new("Captured", "user-1")); // stamped at T0
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

        handle.enqueue(Event::new("Dropped", "user-1"));
        handle.shutdown_blocking(); // deadline already past -> drop, do not send

        mock.assert_hits(0);
        assert_eq!(
            handle.pending(),
            0,
            "dropped events leave nothing in flight"
        );
    }

    #[test]
    fn historical_batch_sends_chunked() {
        // A historical batch takes its own path: queued off the live buffer and
        // sent in its own chunks (forced out here by a flush), never via the buffer.
        let server = MockServer::start();
        let mock = ok_mock(&server);
        let clock = ManualClock::new();
        let handle = TransportHandle::spawn_with_clock(
            options(server.base_url())
                .max_batch_size(2usize)
                .build()
                .unwrap(),
            Arc::new(clock.clone()),
        );

        handle.enqueue_historical(vec![
            Event::new("H1", "user-1"),
            Event::new("H2", "user-1"),
            Event::new("H3", "user-1"),
        ]);
        handle.flush_blocking(); // flush forces the queued historical batch out

        mock.assert_hits(2); // 3 events / max_batch_size 2 -> two requests
        assert_eq!(handle.pending(), 0, "all historical events delivered");
        handle.shutdown_blocking();
    }

    #[test]
    fn historical_batch_respects_shutdown_timeout() {
        // A historical batch queued before a zero-timeout shutdown is abandoned
        // (not POSTed) like buffered live events — it can't bypass shutdown_timeout
        // by being sent eagerly. The worker waits on the interval, so the Shutdown
        // is observed before the historical batch would be drained.
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

        handle.enqueue_historical(vec![Event::new("H", "user-1")]);
        handle.shutdown_blocking(); // deadline already past -> drop, do not send

        mock.assert_hits(0);
        assert_eq!(
            handle.pending(),
            0,
            "historical events abandoned under a zero shutdown timeout"
        );
    }

    #[test]
    fn historical_batch_flushes_at_size_threshold() {
        // Historical honors `flush_at` like the live buffer: once enough events
        // have queued they're sent right away (here, on enqueue), not held until
        // the interval. `options()` uses a long interval, so only the threshold
        // can drive this send.
        let server = MockServer::start();
        let mock = ok_mock(&server);
        let clock = ManualClock::new();
        let handle = TransportHandle::spawn_with_clock(
            options(server.base_url()).flush_at(2usize).build().unwrap(),
            Arc::new(clock.clone()),
        );

        handle.enqueue_historical(vec![Event::new("H1", "user-1"), Event::new("H2", "user-1")]);
        handle.tick(); // sync only; the size threshold already drained it on enqueue

        mock.assert_hits(1);
        assert_eq!(
            handle.pending(),
            0,
            "threshold-sized historical batch delivered"
        );
        handle.shutdown_blocking();
    }
}

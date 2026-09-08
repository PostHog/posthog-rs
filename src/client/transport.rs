//! Runtime-independent event transport.
//!
//! One background `std::thread` per lane drains a channel, batches events,
//! sends them with **blocking** reqwest, and retries transient failures on a
//! schedule. Being a plain thread with a blocking client (never a tokio task) it
//! works for the async client, the blocking client, and a `std::panic` hook
//! with no runtime present.
//!
//! There are two lanes, described by [`LaneConfig`]: analytics (always
//! spawned) and AI (spawned on first `capture_ai*`). They share this worker and
//! pipeline implementation and differ only in endpoint, compression, and how a
//! batch is bounded — by event count (analytics) or by serialized bytes with a
//! per-event ceiling (AI).
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

use super::capture::{build_event_at, chunk_by_bytes, measure_event, EventSize};
use super::common::{apply_on_error_hooks, preprocess_capture_event};
use super::{CaptureCompression, CaptureDefaults, CaptureFailure, ClientOptions, PostHogError};
use crate::capture_event::CaptureEvent;
use crate::endpoints::Endpoint;
use crate::error::Error;
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
    /// Like [`Control::Flush`], but sends the freshly captured live buffer
    /// *before* older held retries. Used by the panic hook so a retry backlog
    /// behind a slow endpoint can't starve the just-enqueued `$exception` within
    /// the hook's short bounded wait.
    FlushCaptures(Completion),
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
    /// Wall-clock time: stamps each event's capture (enqueue) timestamp, plus the
    /// v0 batch `sent_at` and v1 `created_at` / request headers.
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
    /// The worker thread's id, so teardown and the panic hook can avoid waiting
    /// for or joining this client's own worker.
    worker_id: Option<std::thread::ThreadId>,
}

/// Name of the analytics worker thread (aids debugging). The panic hook detects
/// the worker by its thread *id* (see `worker_id`), not this shared name.
const WORKER_THREAD_NAME: &str = "posthog-transport";

/// Name of the AI-lane worker thread.
const AI_WORKER_THREAD_NAME: &str = "posthog-transport-ai";

/// Soft byte target for one AI batch body. The capture-ai deployment caps a
/// compressed request body at 20 MiB; with the guarded check-before-append rule
/// the worst-case body is `max(target, largest event)` = 8 MiB, a 2.5x margin.
/// Same value posthog-python and posthog-node use.
pub(crate) const AI_BATCH_BYTES_TARGET: usize = 5 * 1024 * 1024;

/// Per-event ceiling on the serialized `properties` object of an AI event,
/// mirroring the backend's `AI_MAX_EVENT_BYTES` (strictly greater is refused
/// with `ai_event_too_big`). Events over it are dropped locally so a doomed
/// multi-MB upload is never attempted. Measured on `properties` only, like the
/// v1 backend — not on the whole serialized event as the v0 path did.
pub(crate) const AI_MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

/// AI events are multi-KB to multi-MB JSON; zstd is the best codec the v1
/// endpoint accepts, so the lane pins it regardless of `capture_compression`.
pub(crate) const AI_COMPRESSION: CaptureCompression = CaptureCompression::Zstd;

/// Everything that differs between the analytics and AI transport lanes. One
/// worker/pipeline implementation serves both; a lane is chosen by the public
/// method the caller used, never by an argument on it.
#[derive(Debug, Clone)]
pub(crate) struct LaneConfig {
    pub(crate) endpoint: Endpoint,
    pub(crate) thread_name: &'static str,
    /// `Some`: force this codec. `None`: use `ClientOptions::capture_compression`.
    pub(crate) compression_override: Option<CaptureCompression>,
    /// `Some`: byte-based batching with this soft target (events are built and
    /// measured when buffered). `None`: count-based only, built at send time.
    pub(crate) batch_bytes_target: Option<usize>,
    /// `Some`: drop an event locally when its serialized `properties` exceed
    /// this many bytes. `None`: no local ceiling.
    pub(crate) max_event_bytes: Option<usize>,
}

impl LaneConfig {
    /// Today's analytics behavior, unchanged: count-based batches built at send
    /// time, client-configured compression, no local size ceiling.
    pub(crate) fn analytics() -> Self {
        Self {
            endpoint: Endpoint::Capture,
            thread_name: WORKER_THREAD_NAME,
            compression_override: None,
            batch_bytes_target: None,
            max_event_bytes: None,
        }
    }

    pub(crate) fn ai() -> Self {
        Self {
            endpoint: Endpoint::CaptureAi,
            thread_name: AI_WORKER_THREAD_NAME,
            compression_override: Some(AI_COMPRESSION),
            batch_bytes_target: Some(AI_BATCH_BYTES_TARGET),
            max_event_bytes: Some(AI_MAX_EVENT_BYTES),
        }
    }
}

/// Upper bound on a blocking-flush / shutdown timeout, so an absurd value can't
/// overflow the `now + timeout` deadlines (worker drain) or the `recv_timeout`
/// bound on `flush_blocking_timeout`.
const MAX_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(86_400);

impl TransportHandle {
    /// Spawn the analytics worker with the real system clock.
    pub(crate) fn spawn(options: ClientOptions) -> Self {
        Self::spawn_lane(options, LaneConfig::analytics())
    }

    /// Spawn a worker for `lane` with the real system clock.
    pub(crate) fn spawn_lane(options: ClientOptions, lane: LaneConfig) -> Self {
        Self::spawn_lane_with_clock(options, lane, Arc::new(SystemClock))
    }

    /// Test seam: the analytics lane on an injected clock.
    #[cfg(test)]
    fn spawn_with_clock(options: ClientOptions, clock: Arc<dyn Clock>) -> Self {
        Self::spawn_lane_with_clock(options, LaneConfig::analytics(), clock)
    }

    fn spawn_lane_with_clock(
        options: ClientOptions,
        lane: LaneConfig,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Control>();
        let len = Arc::new(AtomicUsize::new(0));
        let max_queue_size = options.max_queue_size;
        let worker_len = len.clone();
        let worker_clock = Arc::clone(&clock);
        let worker = thread::Builder::new()
            .name(lane.thread_name.to_string())
            .spawn(move || run_worker(options, lane, rx, worker_len, worker_clock))
            .ok();
        let worker_id = worker.as_ref().map(|handle| handle.thread().id());
        Self {
            tx,
            len,
            closed: AtomicBool::new(false),
            worker: Mutex::new(worker),
            full_warned: AtomicBool::new(false),
            max_queue_size,
            clock,
            worker_id,
        }
    }

    /// Non-blocking enqueue. Drops (with a single warning) when the queue is full
    /// or the client is closed.
    pub(crate) fn enqueue(&self, event: Event) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if try_reserve(&self.len, self.max_queue_size, &self.full_warned) {
            self.send_reserved(event);
        }
    }

    /// Like `enqueue`, but never logs — no full-queue `warn!`. The panic hook
    /// enqueues through this on the *panicking* thread, which must not run
    /// arbitrary tracing-subscriber code (a subscriber could panic or wait on a
    /// lock the panic site holds). A full queue silently drops the `$exception`.
    #[cfg(feature = "error-tracking")]
    pub(crate) fn enqueue_panic(&self, event: Event) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if reserve_slot(&self.len, self.max_queue_size).is_some() {
            self.send_reserved(event);
        }
    }

    /// Stamp the capture (enqueue) time on the producer side — so a batched or
    /// retried event records when it occurred, not when it was finally sent —
    /// then hand it to the worker, releasing the reserved slot if the worker is
    /// gone. The slot must already be reserved by the caller.
    fn send_reserved(&self, mut event: Event) {
        event.ensure_timestamp(self.clock.now_utc());
        if self
            .tx
            .send(Control::Capture {
                event: Box::new(event),
            })
            .is_err()
        {
            // Worker gone; release the slot we reserved.
            dec_len(&self.len, 1);
        }
    }

    /// Enqueue a caller-formed batch according to its ingestion policy. Live
    /// events retain the per-event enqueue behavior; historical events stay
    /// together on the dedicated historical path. Both reserve capacity per
    /// event and may accept only a prefix when the queue fills.
    pub(crate) fn enqueue_batch(&self, events: Vec<Event>, historical_migration: bool) {
        if historical_migration {
            self.enqueue_historical(events);
        } else {
            for event in events {
                self.enqueue(event);
            }
        }
    }

    /// Enqueue a caller-formed historical-migration batch on its own path, kept
    /// off the live buffer (which is always non-historical). Reserves a queue
    /// slot per event up to the bound, dropping any overflow with the usual
    /// once-per-episode full warning.
    fn enqueue_historical(&self, mut events: Vec<Event>) {
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
            dec_len(&self.len, fitted);
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

    /// Synchronously close and join the worker for external callers. When called
    /// from the worker itself (for example, when a callback drops its client),
    /// only queue the shutdown: waiting for its completion or joining here would
    /// deadlock the worker inside its own callback.
    #[cfg(test)]
    pub(crate) fn close_blocking(&self) {
        Self::close_all_blocking(&[self]);
    }

    /// [`close_blocking`](Self::close_blocking) across several lanes at once,
    /// phased so their drains overlap: request every shutdown first, then wait
    /// for each, then join each. Two lanes therefore tear down within one
    /// `shutdown_timeout_ms`, not two. If the caller is on *any* of these
    /// workers, no waiting or joining happens (same rule as the single-lane
    /// form: a worker must not wait on itself inside a callback).
    pub(crate) fn close_all_blocking(handles: &[&TransportHandle]) {
        let on_worker = handles.iter().any(|h| h.on_worker_thread());
        let mut waits = Vec::with_capacity(handles.len());
        for handle in handles {
            if handle.begin_close() {
                let (tx, rx) = mpsc::channel();
                if handle.send_control(Control::Shutdown(Completion::Blocking(tx))) && !on_worker {
                    waits.push(rx);
                }
            }
        }
        for rx in waits {
            let _ = rx.recv();
        }
        if !on_worker {
            for handle in handles {
                handle.join();
            }
        }
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

    /// Blocking flush via an `mpsc` completion. Waits (unbounded) for the worker
    /// to attempt delivery of everything queued, then returns. Test-only; the
    /// panic hook uses `flush_blocking_timeout`.
    #[cfg(test)]
    pub(crate) fn flush_blocking(&self) {
        let (tx, rx) = mpsc::channel();
        if self.send_control(Control::Flush(Completion::Blocking(tx))) {
            let _ = rx.recv();
        }
    }

    /// Blocking flush bounded to at most `timeout` (clamped to
    /// `MAX_SHUTDOWN_TIMEOUT`) — runtime-free via an `mpsc` completion, so it
    /// works from the panic hook (the async client's public `flush` uses a
    /// oneshot). The bound matters there: the hook runs on the panicking thread
    /// before unwinding releases locks, so an unbounded wait would hang the dying
    /// process if a `before_send` hook (run on the worker) needs a lock the panic
    /// site still holds — the worker would block on it forever.
    #[cfg(feature = "error-tracking")]
    pub(crate) fn flush_blocking_timeout(&self, timeout: Duration) {
        let (tx, rx) = mpsc::channel();
        if self.send_control(Control::FlushCaptures(Completion::Blocking(tx))) {
            let _ = rx.recv_timeout(timeout.min(MAX_SHUTDOWN_TIMEOUT));
        }
    }

    /// True when the calling thread is this transport's worker thread. Teardown
    /// must not synchronously wait there; the panic hook also skips capturing to
    /// avoid a self-flush deadlock or recursive `before_send` panic.
    pub(crate) fn on_worker_thread(&self) -> bool {
        self.worker_id == Some(std::thread::current().id())
    }

    /// Test helper: flush + stop + join, mirroring the client's shutdown.
    #[cfg(test)]
    fn shutdown_blocking(&self) {
        self.close_blocking();
    }
}

/// Reserve a queue slot under the bounded-capacity cap, returning
/// `Some(prev_count)` on success or `None` when full. A CAS keeps the count
/// exact under concurrent producers. Pure (no logging), so the panic path can
/// reserve a slot without running any tracing; `try_reserve` layers the
/// full-queue warning on top.
fn reserve_slot(len: &AtomicUsize, max: usize) -> Option<usize> {
    loop {
        let current = len.load(Ordering::Acquire);
        if current >= max {
            return None;
        }
        if len
            .compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Some(current);
        }
    }
}

/// Reserve a queue slot, warning once when full. Re-arms that warning once the
/// queue has fully drained, so a service that repeatedly fills then drains warns
/// once per episode instead of only on the very first overflow.
fn try_reserve(len: &AtomicUsize, max: usize, warned: &AtomicBool) -> bool {
    match reserve_slot(len, max) {
        Some(current) => {
            if current == 0 {
                warned.store(false, Ordering::Release);
            }
            true
        }
        None => {
            if !warned.swap(true, Ordering::AcqRel) {
                warn!("posthog-rs: event queue full (capacity {max}); dropping events");
            }
            false
        }
    }
}

/// Decrement the in-flight counter by `n` (no-op for 0). Called when events reach
/// a terminal outcome (delivered or dropped) so `pending()` reflects everything
/// still in flight: channel + worker buffer + retry queue.
///
/// Saturates at 0 instead of a plain `fetch_sub`. The accounting spans several
/// paths (before_send drops, partial v1 results, terminal outcomes, shutdown
/// drops, channel drain); a double-decrement bug would otherwise underflow this
/// `AtomicUsize` and wrap to a huge value, making the bounded queue look
/// permanently full and silently dropping every later event. A `debug_assert`
/// still surfaces such a bug in tests; release builds clamp rather than wrap.
fn dec_len(len: &AtomicUsize, n: usize) {
    if n == 0 {
        return;
    }
    let _ = len.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        debug_assert!(
            current >= n,
            "posthog-rs: in-flight counter underflow ({} - {})",
            current,
            n
        );
        Some(current.saturating_sub(n))
    });
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

/// The worker's live (non-historical) buffer.
///
/// The analytics lane defers building wire events until send time, so
/// `before_send` runs as late as possible and the hot path stays as it was.
/// The AI lane needs each event's serialized size to place batch boundaries,
/// so it builds and measures at admission and holds ready-to-send wire events
/// plus their running byte total.
enum LiveBuffer {
    Deferred(Vec<Event>),
    Prepared {
        events: Vec<CaptureEvent>,
        bytes: usize,
    },
}

impl LiveBuffer {
    fn for_lane(lane: &LaneConfig) -> Self {
        match lane.batch_bytes_target {
            Some(_) => LiveBuffer::Prepared {
                events: Vec::new(),
                bytes: 0,
            },
            None => LiveBuffer::Deferred(Vec::new()),
        }
    }

    fn len(&self) -> usize {
        match self {
            LiveBuffer::Deferred(events) => events.len(),
            LiveBuffer::Prepared { events, .. } => events.len(),
        }
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn clear(&mut self) {
        match self {
            LiveBuffer::Deferred(events) => events.clear(),
            LiveBuffer::Prepared { events, bytes } => {
                events.clear();
                *bytes = 0;
            }
        }
    }
}

/// Admit one live event to the buffer, flushing when a size threshold is met.
/// Returns `true` when the buffer was sent (so the caller resets its timer).
///
/// Deferred (analytics): push, flush at `flush_at` events — unchanged behavior.
///
/// Prepared (AI): build + measure now (dropping the event if it fails the
/// lane's `properties` ceiling), then apply the guarded check-before-append
/// rule: if the buffer is non-empty and this event would push it past the byte
/// target, send the buffer first and start a new one with this event. That
/// keeps every body at `max(target, largest event)`; an event bigger than the
/// target still ships, alone. After appending, flush when the count reaches
/// `flush_at` or the bytes reach the target, so a full batch does not wait for
/// the interval.
fn admit_live(
    pipeline: &mut Pipeline,
    buffer: &mut LiveBuffer,
    buffer_since: &mut Option<Instant>,
    event: Event,
    flush_at: usize,
    max_batch_size: usize,
) -> bool {
    match buffer {
        LiveBuffer::Deferred(events) => {
            if events.is_empty() {
                *buffer_since = Some(pipeline.clock.now());
            }
            events.push(event);
            if events.len() >= flush_at {
                send_buffer(pipeline, buffer, max_batch_size, None);
                return true;
            }
            false
        }
        LiveBuffer::Prepared { .. } => {
            let target = pipeline
                .lane
                .batch_bytes_target
                .expect("Prepared buffer implies a byte target");
            let Some((wire, size)) = pipeline.prepare_measured(event) else {
                // Dropped locally (before_send or oversize); slot already released.
                return false;
            };
            let mut sent = false;
            if !buffer.is_empty() && buffer_bytes(buffer) + size.total > target {
                send_buffer(pipeline, buffer, max_batch_size, None);
                *buffer_since = None;
                sent = true;
            }
            let LiveBuffer::Prepared { events, bytes } = buffer else {
                unreachable!("buffer variant is fixed per lane");
            };
            if events.is_empty() {
                *buffer_since = Some(pipeline.clock.now());
            }
            events.push(wire);
            *bytes += size.total;
            if events.len() >= flush_at || *bytes >= target {
                send_buffer(pipeline, buffer, max_batch_size, None);
                return true;
            }
            sent
        }
    }
}

fn buffer_bytes(buffer: &LiveBuffer) -> usize {
    match buffer {
        LiveBuffer::Deferred(_) => 0,
        LiveBuffer::Prepared { bytes, .. } => *bytes,
    }
}

fn run_worker(
    options: ClientOptions,
    lane: LaneConfig,
    rx: mpsc::Receiver<Control>,
    len: Arc<AtomicUsize>,
    clock: Arc<dyn Clock>,
) {
    let flush_at = options.flush_at.max(1);
    let max_batch_size = options.max_batch_size.max(1);
    let flush_interval = Duration::from_millis(options.flush_interval_ms);
    // Clamp so an absurd `shutdown_timeout_ms` can't overflow the `now +
    // shutdown_timeout` deadlines below and panic the worker. A day is far beyond
    // any sane teardown budget.
    let shutdown_timeout =
        Duration::from_millis(options.shutdown_timeout_ms).min(MAX_SHUTDOWN_TIMEOUT);
    let mut buffer = LiveBuffer::for_lane(&lane);
    let mut pipeline = Pipeline::new(&options, lane, Arc::clone(&clock), len);

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
                if admit_live(
                    &mut pipeline,
                    &mut buffer,
                    &mut buffer_since,
                    *event,
                    flush_at,
                    max_batch_size,
                ) {
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
            Wake::Msg(Control::FlushCaptures(completion)) => {
                // Panic-hook flush: send the freshly captured live buffer (the
                // `$exception`) FIRST, so a retry backlog behind a slow endpoint
                // can't starve it within the hook's bounded wait. Older retries
                // and historical batches follow, best-effort, on the way out.
                send_buffer(&mut pipeline, &mut buffer, max_batch_size, None);
                buffer_since = None;
                pipeline.flush_retries(None);
                drain_historical(&mut pipeline, &mut historical, None);
                historical_since = None;
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
            Control::Flush(c) | Control::FlushCaptures(c) | Control::Shutdown(c) => c.signal(),
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
    buffer: &mut LiveBuffer,
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
        // The buffer only ever holds live events; historical batches take their
        // own path, so this is always a non-historical send.
        match buffer {
            LiveBuffer::Deferred(events) => {
                let chunk: Vec<Event> = events.drain(..take).collect();
                pipeline.send_batch(chunk, false, deadline);
            }
            LiveBuffer::Prepared { events, bytes } => {
                // A prepared buffer is flushed whole as soon as it reaches the
                // byte target, so every count-chunk drained here is already
                // within it (or is a single over-target event on its own).
                let chunk: Vec<CaptureEvent> = events.drain(..take).collect();
                if events.is_empty() {
                    *bytes = 0;
                }
                pipeline.send_prepared(chunk, false, deadline);
            }
        }
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
// Capture pipeline
// ===========================================================================

use std::collections::HashMap;
use uuid::Uuid;

struct RetryBatch {
    pending: Vec<crate::capture_event::CaptureEvent>,
    request_id: Uuid,
    created_at: String,
    final_results: HashMap<Uuid, crate::capture_event::EventResult>,
    historical_migration: bool,
    attempt: u32,
    next_at: Instant,
}

struct Pipeline {
    http: reqwest::blocking::Client,
    options: ClientOptions,
    lane: LaneConfig,
    url: String,
    clock: Arc<dyn Clock>,
    len: Arc<AtomicUsize>,
    retries: VecDeque<RetryBatch>,
}

impl Pipeline {
    fn new(
        options: &ClientOptions,
        lane: LaneConfig,
        clock: Arc<dyn Clock>,
        len: Arc<AtomicUsize>,
    ) -> Self {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(options.request_timeout_seconds))
            .build()
            .unwrap_or_default();
        let url = options.endpoints().build_url(lane.endpoint);
        Self {
            http,
            options: options.clone(),
            lane,
            url,
            clock,
            len,
            retries: VecDeque::new(),
        }
    }

    /// The codec for this lane's request bodies: the lane's pin if it has one,
    /// otherwise the client's `capture_compression`.
    fn compression(&self) -> Option<CaptureCompression> {
        self.lane
            .compression_override
            .or(self.options.capture_compression)
    }

    /// Send caller-order events that have not been built yet: defaults +
    /// `before_send`, then the wire events. On a byte-batched lane the events
    /// are also measured, checked against the lane's per-event ceiling, and
    /// split into byte-sized requests; otherwise they go as one request.
    fn send_batch(
        &mut self,
        events: Vec<Event>,
        historical_migration: bool,
        deadline: Option<Instant>,
    ) {
        let defaults = self.options.capture_defaults();
        let original = events.len();
        let processed: Vec<Event> = events
            .into_iter()
            .filter_map(|event| {
                preprocess_capture_event(event, &defaults, &self.options.before_send)
            })
            .collect();
        // Events dropped by before_send are terminal.
        dec_len(&self.len, original - processed.len());
        if processed.is_empty() {
            return;
        }
        match self.lane.batch_bytes_target {
            Some(target) => {
                let measured: Vec<(CaptureEvent, usize)> = processed
                    .into_iter()
                    .filter_map(|event| self.build_measured(&event, &defaults))
                    .map(|(wire, size)| (wire, size.total))
                    .collect();
                for chunk in chunk_by_bytes(measured, target) {
                    self.send_prepared(chunk, historical_migration, deadline);
                }
            }
            None => {
                let pending =
                    super::capture::build_events_at(&processed, &defaults, self.clock.now_utc());
                self.send_prepared(pending, historical_migration, deadline);
            }
        }
    }

    /// Send already-built wire events as one request (with retries).
    fn send_prepared(
        &mut self,
        pending: Vec<CaptureEvent>,
        historical_migration: bool,
        deadline: Option<Instant>,
    ) {
        if pending.is_empty() {
            return;
        }
        let batch = RetryBatch {
            pending,
            request_id: Uuid::now_v7(),
            created_at: self.clock.now_utc().to_rfc3339(),
            final_results: HashMap::new(),
            historical_migration,
            attempt: 1,
            next_at: self.clock.now(),
        };
        self.attempt(batch, deadline);
    }

    /// Byte-batched lanes: run defaults + `before_send` on a freshly buffered
    /// live event, then build and measure it. `None` means the event was
    /// dropped locally (hook drop or over the lane's ceiling) and its queue slot
    /// released.
    fn prepare_measured(&self, event: Event) -> Option<(CaptureEvent, EventSize)> {
        let defaults = self.options.capture_defaults();
        let Some(event) = preprocess_capture_event(event, &defaults, &self.options.before_send)
        else {
            dec_len(&self.len, 1);
            return None;
        };
        self.build_measured(&event, &defaults)
    }

    /// Build the wire event and measure it, enforcing the lane's per-event
    /// `properties` ceiling. The event is already preprocessed. An oversize
    /// event is dropped here with one log line naming only the event and its
    /// size: AI payloads may hold unredacted prompts or media that must never
    /// reach the logs. This is a local drop, so `on_error` does not fire (the
    /// backend would have refused it with `ai_event_too_big` anyway).
    fn build_measured(
        &self,
        event: &Event,
        defaults: &CaptureDefaults,
    ) -> Option<(CaptureEvent, EventSize)> {
        let wire = build_event_at(event, defaults, self.clock.now_utc());
        let size = measure_event(&wire);
        if let Some(max) = self.lane.max_event_bytes {
            if size.properties > max {
                warn!(
                    "posthog-rs: dropping event {:?} for {}: properties are {} bytes, over the {} byte limit",
                    wire.event, self.lane.endpoint, size.properties, max
                );
                dec_len(&self.len, 1);
                return None;
            }
        }
        Some((wire, size))
    }

    fn attempt(&mut self, mut batch: RetryBatch, deadline: Option<Instant>) {
        use super::capture::{self, Step};
        use crate::capture_event::{BatchRequestRef, CaptureErrorResponse};

        let req = BatchRequestRef {
            created_at: &batch.created_at,
            historical_migration: batch.historical_migration.then_some(true),
            batch: &batch.pending,
        };
        let payload = match serde_json::to_vec(&req) {
            Ok(p) => p,
            Err(e) => {
                let count = batch.pending.len();
                if self.options.on_error.is_empty() {
                    warn!("posthog-rs: dropping {count} event(s), serialization failed: {e}");
                } else {
                    let err = Error::Serialization(e.to_string());
                    let lost = count + undelivered_results(&batch.final_results);
                    self.fire_capture(&batch, None, Some(&err), None, None, lost);
                }
                dec_len(&self.len, count);
                return;
            }
        };
        let mut headers = capture::build_headers_at(
            &self.options,
            &batch.request_id,
            batch.attempt,
            self.clock.now_utc(),
        );
        let body = capture::maybe_compress(self.compression(), &mut headers, payload);

        let count = batch.pending.len();
        let request = bound_request(
            self.http.post(&self.url).headers(headers).body(body),
            deadline,
            self.clock.now(),
            self.options.request_timeout_seconds,
        );
        // The final attempt's status and (on a non-2xx) raw body, kept so the
        // `on_error` hook can surface them. The body is only retained when a hook
        // is registered, so the common path stays allocation-neutral.
        let mut http_status: Option<u16> = None;
        let mut response_body: Option<String> = None;
        let step = match request.send() {
            Err(e) => capture::after_transport_error(
                &self.options,
                &batch.request_id,
                batch.attempt,
                e.to_string(),
            ),
            Ok(resp) => {
                let status = resp.status().as_u16();
                http_status = Some(status);
                let retry_after = capture::parse_retry_after(resp.headers());
                let text = resp.text().unwrap_or_else(|_| "Unknown error".to_string());
                let step = capture::after_response(
                    &self.options,
                    &batch.request_id,
                    batch.attempt,
                    status,
                    retry_after,
                    &text,
                    &mut batch.pending,
                    &mut batch.final_results,
                );
                if !self.options.on_error.is_empty() && !(200..=299).contains(&status) {
                    response_body = Some(text);
                }
                step
            }
        };

        // Events that left `pending` (the ok/drop/warning subset) are terminal.
        dec_len(&self.len, count - batch.pending.len());

        match step {
            Step::Done => {
                // A 2xx whose per-event verdicts include `drop` or `retry`-on-final
                // (events the backend will not persist): surface them even though
                // the request itself succeeded (`error` is `None`). Without a hook
                // this is one aggregate line per batch — never one per event, so
                // a misrouted integration at volume cannot flood the logs, and
                // never naming payloads. The per-event detail lives on the
                // project's Ingestion Warnings page.
                let lost = batch.pending.len() + undelivered_results(&batch.final_results);
                if lost > 0 {
                    if self.options.on_error.is_empty() {
                        warn!(
                            "posthog-rs: {lost} event(s) not persisted by {} (per-event verdicts)",
                            self.lane.endpoint
                        );
                    } else {
                        self.fire_capture(
                            &batch,
                            Some(&batch.request_id),
                            None,
                            http_status,
                            None,
                            lost,
                        );
                    }
                }
            }
            Step::Fail(e) => {
                if self.options.on_error.is_empty() {
                    warn!(
                        "posthog-rs: dropping {} event(s) for {}: {e}",
                        batch.pending.len(),
                        self.lane.endpoint
                    );
                } else {
                    let error_response = response_body
                        .as_deref()
                        .and_then(|b| serde_json::from_str::<CaptureErrorResponse>(b).ok());
                    let lost = batch.pending.len() + undelivered_results(&batch.final_results);
                    self.fire_capture(
                        &batch,
                        Some(&batch.request_id),
                        Some(&e),
                        http_status,
                        error_response.as_ref(),
                        lost,
                    );
                }
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

    /// Fire the `on_error` hooks for a terminal capture outcome. `error` is
    /// `None` only for a `2xx` whose events weren't persisted after retries.
    fn fire_capture(
        &self,
        batch: &RetryBatch,
        request_id: Option<&Uuid>,
        error: Option<&Error>,
        status: Option<u16>,
        error_response: Option<&crate::capture_event::CaptureErrorResponse>,
        event_count: usize,
    ) {
        let failure = PostHogError::Capture(CaptureFailure {
            endpoint: self.lane.endpoint,
            error,
            status,
            attempt: batch.attempt,
            event_count,
            historical_migration: batch.historical_migration,
            request_id,
            results: &batch.final_results,
            error_response,
        });
        apply_on_error_hooks(&self.options.on_error, &failure);
    }
}

/// Count events the V1 backend will not persist (`retry`/`drop` verdicts).
/// `ok`/`warning` were delivered, so they are excluded from the lost tally.
fn undelivered_results(results: &HashMap<Uuid, crate::capture_event::EventResult>) -> usize {
    use crate::capture_event::EventStatus;
    results
        .values()
        .filter(|r| matches!(r.result, EventStatus::Retry | EventStatus::Drop))
        .count()
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

    #[test]
    fn close_from_worker_callback_does_not_wait_or_self_join() {
        let handle_slot = Arc::new(Mutex::new(None::<Arc<TransportHandle>>));
        let callback_slot = Arc::clone(&handle_slot);
        let (returned_tx, returned_rx) = mpsc::channel();
        let handle = Arc::new(TransportHandle::spawn(
            options("http://localhost:0".to_string())
                .flush_at(1usize)
                .before_send(move |_| {
                    let callback_handle = callback_slot
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .take()
                        .expect("transport installed before capture");
                    assert!(callback_handle.on_worker_thread());
                    callback_handle.close_blocking();
                    returned_tx.send(()).unwrap();
                    None
                })
                .build()
                .unwrap(),
        ));
        *handle_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::clone(&handle));

        handle.enqueue(Event::new("close-in-callback", "user-1"));
        returned_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker-thread close blocked");

        // An external caller still performs the durable join, and repeats are no-ops.
        handle.close_blocking();
        handle.close_blocking();
        assert!(handle.is_closed());
        assert_eq!(handle.pending(), 0);
    }

    #[test]
    fn concurrent_external_close_is_idempotent_and_durable() {
        let server = MockServer::start();
        let mock = ok_mock(&server);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let handle = Arc::new(TransportHandle::spawn(
            options(server.base_url())
                .flush_at(1usize)
                .before_send(move |event| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Some(event)
                })
                .build()
                .unwrap(),
        ));
        handle.enqueue(Event::new("close-race", "user-1"));
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker did not enter callback");

        let start = Arc::new(std::sync::Barrier::new(3));
        let closers: Vec<_> = (0..2)
            .map(|_| {
                let handle = Arc::clone(&handle);
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    handle.close_blocking();
                })
            })
            .collect();
        start.wait();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !handle.is_closed() && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(handle.is_closed(), "neither close caller won the race");
        release_tx.send(()).unwrap();
        for closer in closers {
            closer.join().unwrap();
        }

        handle.close_blocking();
        mock.assert_calls(1);
        assert_eq!(handle.pending(), 0);
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
    fn capture_preprocessing_applies_defaults_before_hooks_and_accounts_for_drops() {
        // before_send drops one of two events; a 503 holds the batch for retry.
        // pending() must reflect only the surviving event: the dropped one is
        // terminal at build time, so counting it as in-flight would inflate the
        // bounded-queue depth (and the drop/retry logs) for the batch's lifetime.
        let server = MockServer::start();
        let fail = server.mock(|when, then| {
            when.method(POST)
                .body_includes("\"hook_saw_defaults\":true");
            then.status(503);
        });
        let clock = ManualClock::new();
        let handle = TransportHandle::spawn_with_clock(
            options(server.base_url())
                .disable_geoip(true)
                .before_send(|mut event| {
                    let saw_defaults = event.properties().get("$is_server")
                        == Some(&serde_json::json!(true))
                        && event.properties().get("$geoip_disable")
                            == Some(&serde_json::json!(true));
                    event
                        .insert_prop("hook_saw_defaults", saw_defaults)
                        .unwrap();
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

    /// Parse an RFC3339 wire timestamp and normalize it to UTC.
    fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(s)
            .map(|d| d.with_timezone(&Utc))
            .ok()
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
                let Ok(json) = serde_json::from_slice::<serde_json::Value>(req.body_ref()) else {
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
    fn enqueue_batch_routes_live_and_historical_policies() {
        let server = MockServer::start();
        let live = server.mock(|when, then| {
            when.method(POST).is_true(|req| {
                serde_json::from_slice::<serde_json::Value>(req.body_ref()).is_ok_and(|body| {
                    body["batch"][0]["event"].as_str() == Some("live")
                        && body.get("historical_migration").is_none()
                })
            });
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({ "results": {} }));
        });
        let historical = server.mock(|when, then| {
            when.method(POST).is_true(|req| {
                serde_json::from_slice::<serde_json::Value>(req.body_ref()).is_ok_and(|body| {
                    body["batch"][0]["event"].as_str() == Some("historical")
                        && body["historical_migration"].as_bool() == Some(true)
                })
            });
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({ "results": {} }));
        });
        let handle = TransportHandle::spawn_with_clock(
            options(server.base_url()).build().unwrap(),
            Arc::new(ManualClock::new()),
        );

        handle.enqueue_batch(vec![Event::new("live", "user-1")], false);
        handle.enqueue_batch(vec![Event::new("historical", "user-1")], true);
        handle.flush_blocking();

        live.assert_calls(1);
        historical.assert_calls(1);
        assert_eq!(handle.pending(), 0);
        handle.shutdown_blocking();
    }

    #[test]
    fn enqueue_batch_preserves_per_event_capacity_for_both_policies() {
        let server = MockServer::start();
        let mock = ok_mock(&server);

        for historical_migration in [false, true] {
            let handle = TransportHandle::spawn_with_clock(
                options(server.base_url())
                    .max_queue_size(2usize)
                    .shutdown_timeout_ms(0u64)
                    .build()
                    .unwrap(),
                Arc::new(ManualClock::new()),
            );

            handle.enqueue_batch(
                vec![
                    Event::new("one", "user-1"),
                    Event::new("two", "user-1"),
                    Event::new("overflow", "user-1"),
                ],
                historical_migration,
            );

            assert_eq!(
                handle.pending(),
                2,
                "each batch policy accepts only the prefix that fits"
            );
            handle.shutdown_blocking();
            assert_eq!(handle.pending(), 0);
        }

        mock.assert_calls(0);
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

    // -- on_error capture hook ----------------------------------------------

    #[test]
    fn capture_failure_hook_fires_on_terminal_reject() {
        // A non-retryable 400 makes the batch terminal on the first attempt: the
        // on_error hook fires exactly once carrying the HTTP status, the lost
        // event count, and the underlying error. Uses only version-agnostic
        // accessors so it holds for both the v0 (/batch) and v1 wire shapes.
        let server = MockServer::start();
        let _bad = server.mock(|when, then| {
            when.method(POST);
            then.status(400).body("bad request");
        });
        let recorded = Arc::new(Mutex::new(Vec::<(Option<u16>, usize, bool, bool)>::new()));
        let sink = recorded.clone();
        let clock = ManualClock::new();
        let handle = TransportHandle::spawn_with_clock(
            options(server.base_url())
                .on_error(move |failure| {
                    if let PostHogError::Capture(c) = failure {
                        sink.lock().unwrap_or_else(|p| p.into_inner()).push((
                            c.status(),
                            c.event_count(),
                            c.historical_migration(),
                            c.error().is_some(),
                        ));
                    }
                })
                .build()
                .unwrap(),
            Arc::new(clock.clone()),
        );

        handle.enqueue(Event::new("e", "user-1"));
        handle.flush_blocking();
        handle.shutdown_blocking();

        let recorded = recorded.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(recorded.len(), 1, "exactly one capture failure expected");
        let (status, count, historical, has_error) = recorded[0];
        assert_eq!(status, Some(400));
        assert_eq!(count, 1);
        assert!(!historical);
        assert!(has_error, "a terminal reject carries the underlying error");
    }

    /// The lost tally must count `retry` and `drop` verdicts (events the backend
    /// will not persist) while excluding delivered `ok`/`warning` — a batch that
    /// mixes a drop with a retry must report both, not just whatever remained in
    /// `pending`. Guards the historical `event_count` under-count.
    #[test]
    fn undelivered_results_counts_retry_and_drop_only() {
        use crate::capture_event::{EventResult, EventStatus};
        let mk = |result| EventResult {
            result,
            details: None,
        };
        let results = HashMap::from([
            (Uuid::now_v7(), mk(EventStatus::Ok)),
            (Uuid::now_v7(), mk(EventStatus::Warning)),
            (Uuid::now_v7(), mk(EventStatus::Retry)),
            (Uuid::now_v7(), mk(EventStatus::Drop)),
        ]);
        assert_eq!(undelivered_results(&results), 2);
    }

    #[test]
    fn capture_failure_surfaces_request_id_and_error_response() {
        // A non-2xx V1 response with a structured error body must reach the hook
        // as a parsed `error_response`, alongside the request id of the attempt.
        let server = MockServer::start();
        let _bad = server.mock(|when, then| {
            when.method(POST);
            then.status(400)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "error": "invalid_payload",
                    "error_description": "malformed batch"
                }));
        });
        let recorded = Arc::new(Mutex::new(Vec::<(bool, Option<String>, bool)>::new()));
        let sink = recorded.clone();
        let clock = ManualClock::new();
        let handle = TransportHandle::spawn_with_clock(
            options(server.base_url())
                .on_error(move |failure| {
                    if let PostHogError::Capture(c) = failure {
                        sink.lock().unwrap_or_else(|p| p.into_inner()).push((
                            c.request_id().is_some(),
                            c.error_response().map(|er| er.error.clone()),
                            c.error().is_some(),
                        ));
                    }
                })
                .build()
                .unwrap(),
            Arc::new(clock.clone()),
        );

        handle.enqueue(Event::new("e", "user-1"));
        handle.flush_blocking();
        handle.shutdown_blocking();

        let recorded = recorded.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(recorded.len(), 1);
        let (has_request_id, error_kind, has_error) = &recorded[0];
        assert!(has_request_id, "v1 failures carry the attempt request id");
        assert_eq!(error_kind.as_deref(), Some("invalid_payload"));
        assert!(has_error);
    }

    #[test]
    fn capture_failure_2xx_counts_dropped_and_final_retry() {
        // A 2xx whose per-event verdicts leave events un-persisted (a `drop` and
        // a `retry` on the final attempt) fires the hook once as `Step::Done`
        // with `error == None`, and `event_count` counts BOTH lost events while
        // `event_results` carries all verdicts (including the persisted `ok`).
        // End-to-end through the transport — not a unit test of the helper.
        let u1 = Uuid::now_v7();
        let u2 = Uuid::now_v7();
        let u3 = Uuid::now_v7();
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "results": {
                        u1.to_string(): { "result": "ok" },
                        u2.to_string(): { "result": "drop", "details": "not_persisted" },
                        u3.to_string(): { "result": "retry", "details": "not_persisted" }
                    }
                }));
        });

        let recorded = Arc::new(Mutex::new(Vec::<(Option<u16>, bool, usize, usize)>::new()));
        let sink = recorded.clone();
        let clock = ManualClock::new();
        let handle = TransportHandle::spawn_with_clock(
            options(server.base_url())
                .max_capture_attempts(1u32)
                .on_error(move |failure| {
                    if let PostHogError::Capture(c) = failure {
                        sink.lock().unwrap_or_else(|p| p.into_inner()).push((
                            c.status(),
                            c.error().is_some(),
                            c.event_count(),
                            c.event_results().len(),
                        ));
                    }
                })
                .build()
                .unwrap(),
            Arc::new(clock.clone()),
        );

        let mut e1 = Event::new("e1", "user-1");
        e1.set_uuid(u1);
        let mut e2 = Event::new("e2", "user-1");
        e2.set_uuid(u2);
        let mut e3 = Event::new("e3", "user-1");
        e3.set_uuid(u3);
        handle.enqueue(e1);
        handle.enqueue(e2);
        handle.enqueue(e3);
        handle.flush_blocking();
        handle.shutdown_blocking();

        let recorded = recorded.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(recorded.len(), 1, "fires exactly once for the lost batch");
        let (status, has_error, lost, results) = recorded[0];
        assert_eq!(status, Some(200));
        assert!(!has_error, "a 2xx is not an error");
        assert_eq!(lost, 2, "counts the dropped event and the final retry");
        assert_eq!(results, 3, "all verdicts reported, including the ok");
    }

    // -- AI lane -------------------------------------------------------------

    mod ai_lane {
        use super::*;
        use crate::capture_event::EventStatus;
        use crate::endpoints::{CAPTURE_AI_PATH, CAPTURE_PATH};

        /// Mock one path, 200 with an empty verdict map.
        fn path_mock<'a>(server: &'a MockServer, path: &str) -> httpmock::Mock<'a> {
            let path = path.to_string();
            server.mock(move |when, then| {
                when.method(POST).path(path);
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({ "results": {} }));
            })
        }

        /// Mock one path, matching only when the (uncompressed) body contains
        /// every `present` string and none of the `absent` ones.
        fn body_mock<'a>(
            server: &'a MockServer,
            path: &str,
            present: &[&str],
            absent: &[&str],
        ) -> httpmock::Mock<'a> {
            let path = path.to_string();
            let present: Vec<String> = present.iter().map(|s| s.to_string()).collect();
            let absent: Vec<String> = absent.iter().map(|s| s.to_string()).collect();
            server.mock(move |when, then| {
                when.method(POST).path(path).is_true(move |req| {
                    let body = String::from_utf8_lossy(req.body_ref());
                    present.iter().all(|s| body.contains(s.as_str()))
                        && absent.iter().all(|s| !body.contains(s.as_str()))
                });
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({ "results": {} }));
            })
        }

        /// The AI lane with compression off, so body matchers can read the JSON.
        fn plain_ai() -> LaneConfig {
            LaneConfig {
                compression_override: None,
                ..LaneConfig::ai()
            }
        }

        fn spawn(builder: &mut ClientOptionsBuilder, lane: LaneConfig) -> TransportHandle {
            TransportHandle::spawn_lane_with_clock(
                builder.build().unwrap(),
                lane,
                Arc::new(ManualClock::new()),
            )
        }

        /// An event whose `blob` property is `bytes` ASCII characters.
        fn sized_event(name: &str, bytes: usize) -> Event {
            let mut event = Event::new(name, "user-1");
            event.insert_prop("blob", "x".repeat(bytes)).unwrap();
            event
        }

        /// Serialized wire size of `event` as the worker will measure it. Sizes
        /// are deterministic: the timestamp and uuid are fixed-width.
        fn wire_total(event: &Event, builder: &mut ClientOptionsBuilder) -> usize {
            let opts = builder.build().unwrap();
            measure_event(&build_event_at(event, &opts.capture_defaults(), Utc::now())).total
        }

        fn worker_thread_name(handle: &TransportHandle) -> Option<String> {
            handle
                .worker
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|h| h.thread().name().map(str::to_string))
        }

        #[test]
        fn lane_configs_pin_the_decided_limits() {
            let ai = LaneConfig::ai();
            assert_eq!(ai.endpoint, Endpoint::CaptureAi);
            assert_eq!(ai.compression_override, Some(CaptureCompression::Zstd));
            assert_eq!(ai.batch_bytes_target, Some(5 * 1024 * 1024));
            assert_eq!(ai.max_event_bytes, Some(8 * 1024 * 1024));
            let analytics = LaneConfig::analytics();
            assert_eq!(analytics.endpoint, Endpoint::Capture);
            assert_eq!(analytics.compression_override, None);
            assert_eq!(analytics.batch_bytes_target, None);
            assert_eq!(analytics.max_event_bytes, None);
        }

        #[test]
        fn ai_lane_posts_zstd_to_ai_path_and_analytics_lane_is_unchanged() {
            // One server, both lanes. The AI lane must hit `/i/v1/ai/events` with a
            // zstd body regardless of the client's compression setting; the
            // analytics lane keeps the client's setting (none here) and path.
            let server = MockServer::start();
            let ai_mock = server.mock(|when, then| {
                when.method(POST)
                    .path(CAPTURE_AI_PATH)
                    .header("content-encoding", "zstd")
                    .is_true(|req| {
                        let body = zstd::decode_all(req.body_ref()).expect("zstd body");
                        String::from_utf8_lossy(&body).contains("\"event\":\"$ai_generation\"")
                    });
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({ "results": {} }));
            });
            let analytics_mock = server.mock(|when, then| {
                when.method(POST)
                    .path(CAPTURE_PATH)
                    .header_missing("content-encoding")
                    .body_includes("\"event\":\"clicked\"");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({ "results": {} }));
            });

            let analytics = spawn(&mut options(server.base_url()), LaneConfig::analytics());
            let ai = spawn(&mut options(server.base_url()), LaneConfig::ai());
            analytics.enqueue(Event::new("clicked", "user-1"));
            ai.enqueue(Event::new("$ai_generation", "user-1"));
            analytics.flush_blocking();
            ai.flush_blocking();

            analytics_mock.assert_calls(1);
            ai_mock.assert_calls(1);
            assert_eq!(
                worker_thread_name(&ai).as_deref(),
                Some(AI_WORKER_THREAD_NAME)
            );
            assert_eq!(
                worker_thread_name(&analytics).as_deref(),
                Some(WORKER_THREAD_NAME)
            );
            analytics.shutdown_blocking();
            ai.shutdown_blocking();
            TransportHandle::close_all_blocking(&[&analytics, &ai]);
        }

        #[test]
        fn client_compression_applies_to_analytics_but_not_to_ai_lane() {
            let server = MockServer::start();
            let ai_mock = server.mock(|when, then| {
                when.method(POST)
                    .path(CAPTURE_AI_PATH)
                    .header("content-encoding", "zstd");
                then.status(200)
                    .json_body(serde_json::json!({ "results": {} }));
            });
            let analytics_mock = server.mock(|when, then| {
                when.method(POST)
                    .path(CAPTURE_PATH)
                    .header("content-encoding", "gzip");
                then.status(200)
                    .json_body(serde_json::json!({ "results": {} }));
            });
            let mut builder = options(server.base_url());
            builder.capture_compression(CaptureCompression::Gzip);
            let analytics = spawn(&mut builder, LaneConfig::analytics());
            let ai = spawn(&mut builder, LaneConfig::ai());
            analytics.enqueue(Event::new("clicked", "user-1"));
            ai.enqueue(Event::new("$ai_generation", "user-1"));
            analytics.flush_blocking();
            ai.flush_blocking();
            analytics_mock.assert_calls(1);
            ai_mock.assert_calls(1);
            TransportHandle::close_all_blocking(&[&analytics, &ai]);
        }

        #[test]
        fn byte_target_closes_batch_before_append_and_carries_the_event_over() {
            // Three equal events, target between 2S and 3S: admitting e3 closes
            // [e1, e2] and starts a new batch with e3, which the flush then sends.
            let server = MockServer::start();
            let first = body_mock(&server, CAPTURE_AI_PATH, &["\"e1\"", "\"e2\""], &["\"e3\""]);
            let second = body_mock(&server, CAPTURE_AI_PATH, &["\"e3\""], &["\"e1\"", "\"e2\""]);
            let mut builder = options(server.base_url());
            let size = wire_total(&sized_event("e1", 100), &mut builder);
            let lane = LaneConfig {
                batch_bytes_target: Some(2 * size + size / 2),
                ..plain_ai()
            };
            let handle = spawn(&mut builder, lane);
            handle.enqueue(sized_event("e1", 100));
            handle.enqueue(sized_event("e2", 100));
            handle.enqueue(sized_event("e3", 100));
            handle.tick(); // worker has admitted all three
            first.assert_calls(1);
            second.assert_calls(0);
            assert_eq!(handle.pending(), 1, "e3 carried over into the next batch");
            handle.flush_blocking();
            second.assert_calls(1);
            assert_eq!(handle.pending(), 0);
            TransportHandle::close_all_blocking(&[&handle]);
        }

        #[test]
        fn event_exactly_at_target_flushes_alone_without_waiting() {
            let server = MockServer::start();
            let mock = path_mock(&server, CAPTURE_AI_PATH);
            let mut builder = options(server.base_url());
            let size = wire_total(&sized_event("e1", 100), &mut builder);
            let lane = LaneConfig {
                batch_bytes_target: Some(size),
                ..plain_ai()
            };
            let handle = spawn(&mut builder, lane);
            handle.enqueue(sized_event("e1", 100));
            handle.tick();
            mock.assert_calls(1);
            assert_eq!(handle.pending(), 0);
            TransportHandle::close_all_blocking(&[&handle]);
        }

        #[test]
        fn event_larger_than_target_ships_alone_after_the_current_batch() {
            // A small event is buffered; a big one (over the target by itself) must
            // first force the small batch out, then go on its own — in that order.
            let server = MockServer::start();
            let small = body_mock(&server, CAPTURE_AI_PATH, &["\"small\""], &["\"big\""]);
            let big = body_mock(&server, CAPTURE_AI_PATH, &["\"big\""], &["\"small\""]);
            let mut builder = options(server.base_url());
            let small_size = wire_total(&sized_event("small", 100), &mut builder);
            let lane = LaneConfig {
                batch_bytes_target: Some(2 * small_size + small_size / 2),
                max_event_bytes: None,
                ..plain_ai()
            };
            let handle = spawn(&mut builder, lane);
            handle.enqueue(sized_event("small", 100));
            handle.enqueue(sized_event("big", 10 * small_size));
            handle.tick();
            small.assert_calls(1);
            big.assert_calls(1);
            assert_eq!(handle.pending(), 0);
            TransportHandle::close_all_blocking(&[&handle]);
        }

        #[test]
        fn count_and_interval_triggers_still_apply_under_a_large_byte_target() {
            let server = MockServer::start();
            let mock = path_mock(&server, CAPTURE_AI_PATH);
            let clock = ManualClock::new();
            let mut builder = options(server.base_url());
            builder.flush_at(2usize);
            let handle = TransportHandle::spawn_lane_with_clock(
                builder.build().unwrap(),
                plain_ai(),
                Arc::new(clock.clone()),
            );
            // Count: two events reach flush_at and go out at once.
            handle.enqueue(Event::new("$ai_span", "user-1"));
            handle.enqueue(Event::new("$ai_span", "user-1"));
            handle.tick();
            mock.assert_calls(1);
            // Interval: one event waits, then goes out when the interval elapses.
            handle.enqueue(Event::new("$ai_span", "user-1"));
            handle.tick();
            mock.assert_calls(1);
            clock.advance(Duration::from_millis(10_000));
            handle.tick();
            mock.assert_calls(2);
            assert_eq!(handle.pending(), 0);
            TransportHandle::close_all_blocking(&[&handle]);
        }

        #[test]
        fn oversize_event_is_dropped_locally_with_no_request_and_no_hook() {
            let server = MockServer::start();
            let mock = path_mock(&server, CAPTURE_AI_PATH);
            let hook_calls = Arc::new(AtomicUsize::new(0));
            let counter = hook_calls.clone();
            let mut builder = options(server.base_url());
            builder.on_error(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            });
            let lane = LaneConfig {
                max_event_bytes: Some(1024),
                ..plain_ai()
            };
            let handle = spawn(&mut builder, lane);
            handle.enqueue(sized_event("$ai_generation", 2048));
            handle.flush_blocking();
            mock.assert_calls(0);
            assert_eq!(handle.pending(), 0, "the dropped event released its slot");
            assert_eq!(
                hook_calls.load(Ordering::SeqCst),
                0,
                "local drops do not fire on_error"
            );
            TransportHandle::close_all_blocking(&[&handle]);
        }

        #[test]
        fn oversize_ceiling_is_measured_on_properties_after_before_send() {
            // The hook shrinks an over-limit event below the ceiling: it is sent.
            let server = MockServer::start();
            let mock = path_mock(&server, CAPTURE_AI_PATH);
            let mut builder = options(server.base_url());
            builder.before_send(|mut event| {
                event.remove_prop("blob");
                Some(event)
            });
            let lane = LaneConfig {
                max_event_bytes: Some(1024),
                ..plain_ai()
            };
            let handle = spawn(&mut builder, lane);
            handle.enqueue(sized_event("$ai_generation", 4096));
            handle.flush_blocking();
            mock.assert_calls(1);
            handle.shutdown_blocking();

            // The hook grows an event past the ceiling: it is dropped.
            let mut builder = options(server.base_url());
            builder.before_send(|mut event| {
                event.insert_prop("padding", "y".repeat(4096)).unwrap();
                Some(event)
            });
            let handle = spawn(
                &mut builder,
                LaneConfig {
                    max_event_bytes: Some(1024),
                    ..plain_ai()
                },
            );
            handle.enqueue(Event::new("$ai_generation", "user-1"));
            handle.flush_blocking();
            mock.assert_calls(1);
            assert_eq!(handle.pending(), 0);
            handle.shutdown_blocking();
        }

        #[test]
        fn before_send_drop_at_admission_releases_the_slot() {
            let server = MockServer::start();
            let mock = path_mock(&server, CAPTURE_AI_PATH);
            let mut builder = options(server.base_url());
            builder.before_send(|_| None);
            let handle = spawn(&mut builder, plain_ai());
            handle.enqueue(Event::new("$ai_generation", "user-1"));
            handle.flush_blocking();
            mock.assert_calls(0);
            assert_eq!(handle.pending(), 0);
            TransportHandle::close_all_blocking(&[&handle]);
        }

        #[test]
        fn historical_batch_on_ai_lane_is_chunked_by_bytes() {
            let server = MockServer::start();
            let mock = path_mock(&server, CAPTURE_AI_PATH);
            let mut builder = options(server.base_url());
            let size = wire_total(&sized_event("h1", 100), &mut builder);
            let lane = LaneConfig {
                batch_bytes_target: Some(2 * size + size / 2),
                ..plain_ai()
            };
            let handle = spawn(&mut builder, lane);
            handle.enqueue_batch(
                vec![
                    sized_event("h1", 100),
                    sized_event("h2", 100),
                    sized_event("h3", 100),
                ],
                true,
            );
            handle.flush_blocking();
            mock.assert_calls(2); // [h1, h2] then [h3]
            assert_eq!(handle.pending(), 0);
            TransportHandle::close_all_blocking(&[&handle]);
        }

        #[test]
        fn retry_state_is_isolated_between_lanes() {
            // The AI endpoint fails with a retryable 503 while analytics succeeds:
            // the analytics batch is delivered and the AI batch alone is held.
            let server = MockServer::start();
            let ai_fail = server.mock(|when, then| {
                when.method(POST).path(CAPTURE_AI_PATH);
                then.status(503);
            });
            let analytics_ok = path_mock(&server, CAPTURE_PATH);
            let mut builder = options(server.base_url());
            builder.max_capture_attempts(3u32);
            let analytics = spawn(&mut builder, LaneConfig::analytics());
            let ai = spawn(&mut builder, LaneConfig::ai());
            analytics.enqueue(Event::new("clicked", "user-1"));
            ai.enqueue(Event::new("$ai_generation", "user-1"));
            analytics.flush_blocking();
            ai.flush_blocking();
            analytics_ok.assert_calls(1);
            ai_fail.assert_calls(1);
            assert_eq!(analytics.pending(), 0);
            assert_eq!(ai.pending(), 1, "held for retry on the AI lane only");
            TransportHandle::close_all_blocking(&[&analytics, &ai]);
        }

        #[test]
        fn capture_failure_reports_the_lane_endpoint() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(POST);
                then.status(500);
            });
            let seen = Arc::new(Mutex::new(Vec::<Endpoint>::new()));
            let sink = seen.clone();
            let mut builder = options(server.base_url());
            builder.max_capture_attempts(1u32).on_error(move |failure| {
                if let PostHogError::Capture(c) = failure {
                    sink.lock().unwrap().push(c.endpoint());
                }
            });
            let analytics = spawn(&mut builder, LaneConfig::analytics());
            let ai = spawn(&mut builder, LaneConfig::ai());
            analytics.enqueue(Event::new("clicked", "user-1"));
            analytics.flush_blocking();
            ai.enqueue(Event::new("$ai_generation", "user-1"));
            ai.flush_blocking();
            assert_eq!(
                *seen.lock().unwrap(),
                vec![Endpoint::Capture, Endpoint::CaptureAi]
            );
            TransportHandle::close_all_blocking(&[&analytics, &ai]);
        }

        #[test]
        fn server_verdicts_on_ai_lane_reach_on_error_with_details() {
            // The SDK never tests the event name; the backend does, and its
            // per-event `drop` verdicts (misrouted name, oversize) must reach the
            // hook with their detail strings intact and the AI endpoint named.
            let misrouted = Uuid::now_v7();
            let too_big = Uuid::now_v7();
            let ok = Uuid::now_v7();
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(POST).path(CAPTURE_AI_PATH);
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({ "results": {
                        misrouted.to_string(): { "result": "drop", "details": "non_ai_event" },
                        too_big.to_string(): { "result": "drop", "details": "ai_event_too_big" },
                        ok.to_string(): { "result": "ok" }
                    }}));
            });
            let seen = Arc::new(Mutex::new(
                Vec::<(Endpoint, usize, Vec<(Uuid, String)>)>::new(),
            ));
            let sink = seen.clone();
            let mut builder = options(server.base_url());
            builder.on_error(move |failure| {
                if let PostHogError::Capture(c) = failure {
                    let mut drops: Vec<(Uuid, String)> = c
                        .event_results()
                        .iter()
                        .filter(|(_, r)| r.result == EventStatus::Drop)
                        .map(|(u, r)| (*u, r.details.clone().unwrap_or_default()))
                        .collect();
                    drops.sort();
                    sink.lock()
                        .unwrap()
                        .push((c.endpoint(), c.event_count(), drops));
                }
            });
            let handle = spawn(&mut builder, LaneConfig::ai());
            for (uuid, name) in [
                (misrouted, "not_an_ai_event"),
                (too_big, "$ai_generation"),
                (ok, "$ai_span"),
            ] {
                let mut event = Event::new(name, "user-1");
                event.set_uuid(uuid);
                handle.enqueue(event);
            }
            handle.flush_blocking();

            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            let (endpoint, lost, drops) = &seen[0];
            assert_eq!(*endpoint, Endpoint::CaptureAi);
            assert_eq!(*lost, 2);
            let mut expected = vec![
                (misrouted, "non_ai_event".to_string()),
                (too_big, "ai_event_too_big".to_string()),
            ];
            expected.sort();
            assert_eq!(*drops, expected);
            assert_eq!(handle.pending(), 0);
            TransportHandle::close_all_blocking(&[&handle]);
        }

        #[test]
        fn two_xx_drops_without_a_hook_release_slots() {
            // The no-hook path logs one aggregate line per batch (not asserted) and
            // must still account the dropped events as terminal.
            let dropped = Uuid::now_v7();
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(POST).path(CAPTURE_AI_PATH);
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({ "results": {
                        dropped.to_string(): { "result": "drop", "details": "non_ai_event" }
                    }}));
            });
            let handle = spawn(&mut options(server.base_url()), LaneConfig::ai());
            let mut event = Event::new("not_an_ai_event", "user-1");
            event.set_uuid(dropped);
            handle.enqueue(event);
            handle.flush_blocking();
            assert_eq!(handle.pending(), 0);
            TransportHandle::close_all_blocking(&[&handle]);
        }

        #[test]
        fn close_all_blocking_tears_down_both_lanes_and_is_idempotent() {
            let server = MockServer::start();
            let ai_mock = path_mock(&server, CAPTURE_AI_PATH);
            let analytics_mock = path_mock(&server, CAPTURE_PATH);
            let analytics = spawn(&mut options(server.base_url()), LaneConfig::analytics());
            let ai = spawn(&mut options(server.base_url()), LaneConfig::ai());
            analytics.enqueue(Event::new("clicked", "user-1"));
            ai.enqueue(Event::new("$ai_generation", "user-1"));
            TransportHandle::close_all_blocking(&[&analytics, &ai]);
            TransportHandle::close_all_blocking(&[&analytics, &ai]);
            analytics_mock.assert_calls(1);
            ai_mock.assert_calls(1);
            assert!(analytics.is_closed() && ai.is_closed());
            // Enqueue after close is a silent no-op on both lanes.
            analytics.enqueue(Event::new("late", "user-1"));
            ai.enqueue(Event::new("late", "user-1"));
            assert_eq!(analytics.pending() + ai.pending(), 0);
        }
    }
}

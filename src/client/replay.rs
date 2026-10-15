//! In-memory session replay delivery for native embedding.
//!
//! Frames bypass the [`EventStore`](crate::embedding::EventStore) and the
//! `/batch/` worker: a dedicated thread posts gzipped JSON arrays to `/s/`, one
//! `$session_id` per request. Consent and the shutdown deadline come from the
//! shared embedding [`Gate`].

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use reqwest::header::{CONTENT_ENCODING, CONTENT_TYPE, USER_AGENT};
use tracing::warn;

use super::embedding::Gate;
use super::retry::{parse_retry_after, v0_after_response, v0_after_transport_error, Step};
use super::transport::{bound_request, build_http};
use super::ClientOptions;
use crate::embedding::{EnqueueResult, ReplayOptions};
use crate::endpoints::Endpoint;
use crate::event::InnerEvent;
use crate::Event;

const WORKER_THREAD_NAME: &str = "posthog-replay";

struct Frame {
    session_id: String,
    generation: u64,
    json: Vec<u8>,
}

#[derive(Default)]
struct Queue {
    frames: VecDeque<Frame>,
    /// Queued plus in-flight serialized bytes.
    bytes: usize,
    /// When the oldest unsent frame was queued.
    since: Option<Instant>,
    in_flight: usize,
    closing: bool,
    dropped: usize,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub(crate) struct ReplayHandle {
    shared: Arc<Shared>,
    gate: Arc<Gate>,
    api_key: String,
    limits: ReplayOptions,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl ReplayHandle {
    /// `None` when no HTTP client or worker thread can be created.
    pub(crate) fn spawn(
        options: &ClientOptions,
        limits: ReplayOptions,
        gate: Arc<Gate>,
    ) -> Option<Self> {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
        });
        let worker = Worker {
            shared: Arc::clone(&shared),
            gate: Arc::clone(&gate),
            http: build_http(options)?,
            url: options.endpoints().build_url(Endpoint::Snapshots),
            user_agent: options.user_agent(),
            request_timeout: Duration::from_secs(options.request_timeout_seconds).min(gate.budget),
            limits,
            options: options.clone(),
        };
        let worker = thread::Builder::new()
            .name(WORKER_THREAD_NAME.to_string())
            .spawn(move || worker.run())
            .ok()?;
        Some(Self {
            shared,
            gate,
            api_key: options.api_key.clone(),
            limits,
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Serializes on the calling thread so the queue bound is exact. The event
    /// must already carry `$session_id`; frames without one are grouped together.
    pub(crate) fn enqueue(&self, event: Event) -> EnqueueResult {
        let session_id = event
            .properties()
            .get("$session_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let Ok(json) = serde_json::to_vec(&InnerEvent::new(event, self.api_key.clone())) else {
            return EnqueueResult::Disabled;
        };
        // Same lock as analytics admission, so an opt-out cannot interleave
        // between the consent check and the push it is about to purge.
        let _admission = self
            .gate
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut queue = self.shared.lock();
        if queue.closing {
            return EnqueueResult::Closed;
        }
        if !self.gate.enabled.load(Ordering::Acquire) {
            return EnqueueResult::Disabled;
        }
        if queue.bytes.saturating_add(json.len()) > self.limits.max_queue_bytes {
            return EnqueueResult::QueueFull;
        }
        queue.bytes += json.len();
        queue.since.get_or_insert_with(Instant::now);
        queue.frames.push_back(Frame {
            session_id,
            generation: self.gate.generation.load(Ordering::Acquire),
            json,
        });
        self.shared.wake.notify_all();
        EnqueueResult::Accepted
    }

    /// Drop queued frames. A batch already in flight is discarded by the worker
    /// because its consent generation is stale.
    pub(crate) fn purge(&self) {
        let mut queue = self.shared.lock();
        let purged: usize = queue.frames.drain(..).map(|frame| frame.json.len()).sum();
        queue.bytes -= purged;
        queue.since = None;
    }

    /// `(pending, dropped)` frame counts.
    pub(crate) fn report(&self) -> (usize, usize) {
        let queue = self.shared.lock();
        (queue.frames.len() + queue.in_flight, queue.dropped)
    }

    /// Close admission and let the worker drain within the shutdown deadline.
    pub(crate) fn request_close(&self) {
        self.shared.lock().closing = true;
        self.shared.wake.notify_all();
    }

    /// Join the worker. Safe to call repeatedly; call `request_close` first.
    pub(crate) fn join(&self) {
        if let Some(handle) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = handle.join();
        }
    }
}

struct Worker {
    shared: Arc<Shared>,
    gate: Arc<Gate>,
    http: reqwest::blocking::Client,
    url: String,
    user_agent: String,
    request_timeout: Duration,
    limits: ReplayOptions,
    /// Retry budget and backoff, shared with `/batch/`.
    options: ClientOptions,
}

impl Worker {
    fn run(self) {
        let interval = Duration::from_millis(self.limits.flush_interval_ms);
        let mut close_deadline: Option<Instant> = None;
        loop {
            let (batch, closing) = {
                let mut queue = self.shared.lock();
                loop {
                    if queue.closing || queue.bytes >= self.limits.max_batch_bytes {
                        break;
                    }
                    match queue.since {
                        None => {
                            queue = self
                                .shared
                                .wake
                                .wait(queue)
                                .unwrap_or_else(|e| e.into_inner());
                        }
                        Some(since) => {
                            let elapsed = since.elapsed();
                            if elapsed >= interval {
                                break;
                            }
                            queue = self
                                .shared
                                .wake
                                .wait_timeout(queue, interval - elapsed)
                                .unwrap_or_else(|e| e.into_inner())
                                .0;
                        }
                    }
                }
                if queue.frames.is_empty() {
                    if queue.closing {
                        return;
                    }
                    queue.since = None;
                    continue;
                }
                (self.take_batch(&mut queue), queue.closing)
            };
            let deadline = if closing {
                let fallback =
                    *close_deadline.get_or_insert_with(|| Instant::now() + self.gate.budget);
                self.gate.deadline(Some(fallback))
            } else {
                self.gate.deadline(None)
            };
            self.deliver(batch, deadline);
        }
    }

    /// Consecutive frames of the front frame's session, up to `max_batch_bytes`
    /// (always at least one frame).
    fn take_batch(&self, queue: &mut Queue) -> Vec<Frame> {
        let first = queue.frames.pop_front().expect("caller checked non-empty");
        let mut size = first.json.len() + 2;
        let mut batch = vec![first];
        while let Some(next) = queue.frames.front() {
            if next.session_id != batch[0].session_id
                || size + next.json.len() + 1 > self.limits.max_batch_bytes
            {
                break;
            }
            size += next.json.len() + 1;
            batch.extend(queue.frames.pop_front());
        }
        queue.in_flight = batch.len();
        if queue.frames.is_empty() {
            queue.since = None;
        }
        batch
    }

    fn deliver(&self, batch: Vec<Frame>, deadline: Option<Instant>) {
        let bytes: usize = batch.iter().map(|frame| frame.json.len()).sum();
        let generation = self.gate.generation.load(Ordering::Acquire);
        let live: Vec<Frame> = if self.gate.enabled.load(Ordering::Acquire) {
            batch
                .into_iter()
                .filter(|frame| frame.generation == generation)
                .collect()
        } else {
            Vec::new()
        };
        let delivered = live.is_empty() || self.send(&live, deadline);
        let mut queue = self.shared.lock();
        queue.in_flight = 0;
        queue.bytes -= bytes;
        if !delivered {
            queue.dropped += live.len();
        }
    }

    /// Retries like `/batch/` (attempt budget, backoff, `Retry-After`), but
    /// never waits past the deadline. 413 and other terminal statuses drop the
    /// batch. Returns `false` when the frames were dropped.
    fn send(&self, frames: &[Frame], deadline: Option<Instant>) -> bool {
        let mut json =
            Vec::with_capacity(frames.iter().map(|f| f.json.len() + 1).sum::<usize>() + 1);
        json.push(b'[');
        for (index, frame) in frames.iter().enumerate() {
            if index > 0 {
                json.push(b',');
            }
            json.extend_from_slice(&frame.json);
        }
        json.push(b']');
        let request = self
            .http
            .post(&self.url)
            .header(CONTENT_TYPE, "application/json")
            .header(USER_AGENT, &self.user_agent);
        // Always gzip, ignoring `capture_compression` (unset in embedding): `/s/`
        // expects gzipped bodies, as the mobile SDKs send them.
        let request = match crate::compression::gzip(&json) {
            Some(body) => request.header(CONTENT_ENCODING, "gzip").body(body),
            None => request.body(json),
        };
        let mut failure = String::from("shutdown deadline reached");
        let mut attempt = 1;
        loop {
            if !self.gate.enabled.load(Ordering::Acquire) {
                // Opted out mid-delivery: purged, not a delivery failure.
                return true;
            }
            let deadline = self.gate.deadline(deadline);
            let now = Instant::now();
            if deadline.is_some_and(|d| now >= d) {
                break;
            }
            // A byte body clones by reference count, so retries do not copy it.
            let Some(attempt_request) = request.try_clone() else {
                failure = String::from("request body is not reusable");
                break;
            };
            let step =
                match bound_request(attempt_request, deadline, now, self.request_timeout).send() {
                    Err(e) => v0_after_transport_error(&self.options, attempt, e.to_string()),
                    Ok(response) => {
                        let status = response.status().as_u16();
                        let retry_after = parse_retry_after(response.headers());
                        let body = response.text().unwrap_or_default();
                        v0_after_response(&self.options, attempt, status, retry_after, &body)
                    }
                };
            match step {
                Step::Done => return true,
                Step::Fail(e) => {
                    failure = e.to_string();
                    break;
                }
                Step::Backoff(delay) => {
                    if deadline.is_some_and(|d| Instant::now() + delay >= d) {
                        failure = String::from("retry backoff exceeds the shutdown deadline");
                        break;
                    }
                    self.backoff(delay);
                    attempt += 1;
                }
            }
        }
        warn!(
            "posthog-rs: dropping {} replay frame(s): {failure}",
            frames.len()
        );
        false
    }

    /// Wait out `delay`, returning early if close is requested meanwhile so the
    /// next attempt runs against the shutdown deadline.
    fn backoff(&self, delay: Duration) {
        let until = Instant::now() + delay;
        let mut queue = self.shared.lock();
        let closing = queue.closing;
        while queue.closing == closing {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            queue = self
                .shared
                .wake
                .wait_timeout(queue, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;

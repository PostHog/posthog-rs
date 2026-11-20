//! Screenshot delivery on its own lane, as posthog-ios and posthog-android
//! keep a replay queue beside the event queue. The encoder sends frames,
//! failures and flush or shutdown barriers here in order; event delivery never
//! waits on a screenshot upload, nor the reverse.
use flate2::{write::GzEncoder, Compression};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use super::{build_http, ClientOptions, Clock, Control};
use crate::client::common::apply_on_error_hooks;
use crate::client::on_error::ReplayFailure;
use crate::client::retry::{capture_backoff_duration, is_retryable_status, parse_retry_after};
use crate::client::stateful::replay::EncodedFrame;
use crate::client::PostHogError;

pub(crate) const MAX_BODY_BYTES: usize = 25 * 1024 * 1024;
const INTERVAL: Duration = Duration::from_secs(1);
/// Upload attempts per screenshot batch, including the first.
const MAX_ATTEMPTS: u32 = 4;

/// A buffer that refuses to grow past `limit`, so an oversized image or body
/// fails before it is allocated.
pub(crate) struct LimitedBuffer {
    pub(crate) bytes: Vec<u8>,
    pub(crate) limit: u64,
}

impl Write for LimitedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.limit.saturating_sub(self.bytes.len() as u64) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Screenshot data exceeds its byte limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct Queue {
    waiting: Vec<EncodedFrame>,
    since: Option<Instant>,
    retries: VecDeque<Batch>,
    shutdown_reported: bool,
}
struct Batch {
    frames: Vec<EncodedFrame>,
    attempt: u32,
    due: Instant,
}
impl Queue {
    fn push(&mut self, frame: EncodedFrame, now: Instant) {
        if self.waiting.is_empty() {
            self.since = Some(now);
        }
        self.waiting.push(frame);
    }
    fn next_due(&self, closing: Option<Instant>) -> Option<Instant> {
        self.since
            .map(|since| since + INTERVAL)
            .into_iter()
            .chain(self.retries.iter().map(|b| b.due))
            .chain(closing.filter(|_| !self.waiting.is_empty() || !self.retries.is_empty()))
            .min()
    }
    fn take_ready(&mut self, now: Instant, force: bool) -> Vec<Batch> {
        let mut ready = Vec::new();
        let mut retained = VecDeque::new();
        while let Some(batch) = self.retries.pop_front() {
            if batch.due <= now {
                ready.push(batch);
            } else {
                retained.push_back(batch);
            }
        }
        self.retries = retained;
        if !self.waiting.is_empty()
            && (force || self.waiting.len() >= 2 || self.since.is_some_and(|s| now >= s + INTERVAL))
        {
            let mut frames = std::mem::take(&mut self.waiting);
            frames.sort_by_key(|f| f.attribution.sequence);
            self.since = None;
            for frame in frames {
                let append = ready.last_mut().filter(|b| {
                    b.attempt == 0 && b.frames.len() < 2 && same_context(&b.frames[0], &frame)
                });
                if let Some(batch) = append {
                    batch.frames.push(frame);
                } else {
                    ready.push(Batch {
                        frames: vec![frame],
                        attempt: 0,
                        due: now,
                    });
                }
            }
        }
        ready
    }
}
fn same_context(a: &EncodedFrame, b: &EncodedFrame) -> bool {
    a.attribution.distinct_id == b.attribution.distinct_id
        && a.attribution.session_id == b.attribution.session_id
        && a.attribution.endpoint == b.attribution.endpoint
}

pub(crate) fn report(options: &ClientOptions, message: &'static str, status: Option<u16>) {
    if options.on_error.is_empty() {
        tracing::warn!(status, "posthog-rs: {message}");
    } else {
        apply_on_error_hooks(
            &options.on_error,
            &PostHogError::Replay(ReplayFailure { message, status }),
        );
    }
}

/// The replay lane's thread. Dropping the handle does not stop it: the
/// encoder's shutdown barrier does, or the encoder going away.
pub(crate) struct Lane {
    tx: mpsc::Sender<Control>,
    id: ThreadId,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Lane {
    /// Start the lane. `deadline` is the client's shared shutdown deadline.
    pub(super) fn spawn(
        options: ClientOptions,
        clock: Arc<dyn Clock>,
        deadline: Arc<Mutex<Option<Instant>>>,
    ) -> io::Result<Self> {
        let http = build_http(&options)
            .ok_or_else(|| io::Error::other("Cannot build the replay HTTP client"))?;
        let (tx, rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("posthog-replay".into())
            .spawn(move || {
                Sender {
                    options,
                    clock,
                    http,
                    replay: Queue::default(),
                    closing: None,
                }
                .run(&rx, &deadline)
            })?;
        Ok(Self {
            tx,
            id: thread.thread().id(),
            thread: Mutex::new(Some(thread)),
        })
    }

    pub(super) fn sender(&self) -> mpsc::Sender<Control> {
        self.tx.clone()
    }

    pub(super) fn on_thread(&self) -> bool {
        self.id == thread::current().id()
    }

    pub(super) fn is_stopped(&self) -> bool {
        self.thread
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map_or(true, JoinHandle::is_finished)
    }

    pub(super) fn join(&self) {
        if self.on_thread() {
            return;
        }
        let thread = self.thread.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

struct Sender {
    options: ClientOptions,
    clock: Arc<dyn Clock>,
    http: reqwest::blocking::Client,
    replay: Queue,
    /// The shutdown deadline, once one is selected.
    closing: Option<Instant>,
}

impl Sender {
    fn run(mut self, rx: &mpsc::Receiver<Control>, deadline: &Mutex<Option<Instant>>) {
        loop {
            let end = *deadline.lock().unwrap_or_else(|p| p.into_inner());
            if let (Some(end), None) = (end, self.closing) {
                self.close_delivery(end);
            }
            let message = if end.is_some_and(|end| self.clock.now() >= end) {
                // Past the deadline: only drain what the encoder still sends.
                rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected)
            } else {
                match self.replay.next_due(self.closing) {
                    Some(due) => rx.recv_timeout(due.saturating_duration_since(self.clock.now())),
                    // Wake now and then to notice a deadline selected meanwhile.
                    None => rx.recv_timeout(INTERVAL),
                }
            };
            match message {
                Ok(Control::ReplayFrame(frame)) => self.admit_replay(*frame),
                Ok(Control::ReplayFailure(message)) => report(&self.options, message, None),
                Ok(Control::ReplayShutdownAbandoned) => self.report_replay_shutdown(None),
                Ok(Control::Flush(completion) | Control::FlushCaptures(completion)) => {
                    self.service_replay(true, end);
                    completion.signal();
                }
                #[cfg(test)]
                Ok(Control::Tick(completion)) => {
                    self.service_replay(false, end);
                    completion.signal();
                }
                Ok(Control::Shutdown(completion)) => {
                    self.service_replay(true, end);
                    self.abandon_replay();
                    completion.signal();
                    return;
                }
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => self.service_replay(false, end),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        self.abandon_replay();
    }

    fn delivery_deadline(&self, deadline: Option<Instant>) -> Option<Instant> {
        deadline.into_iter().chain(self.closing).min()
    }

    fn admit_replay(&mut self, frame: EncodedFrame) {
        self.replay.push(frame, self.clock.now());
    }
    fn close_delivery(&mut self, deadline: Instant) {
        self.closing = Some(self.closing.map_or(deadline, |old| old.min(deadline)));
        self.service_replay(true, Some(deadline));
    }
    fn report_replay_shutdown(&mut self, status: Option<u16>) {
        if !self.replay.shutdown_reported {
            self.replay.shutdown_reported = true;
            report(
                &self.options,
                "Screenshot delivery abandoned during shutdown",
                status,
            );
        }
    }
    fn abandon_replay(&mut self) {
        if !self.replay.waiting.is_empty() || !self.replay.retries.is_empty() {
            self.report_replay_shutdown(None);
        }
        self.replay.waiting.clear();
        self.replay.retries.clear();
        self.replay.since = None;
    }
    fn service_replay(&mut self, force: bool, deadline: Option<Instant>) {
        let deadline = self.delivery_deadline(deadline);
        if deadline.is_some_and(|end| self.clock.now() >= end) {
            self.abandon_replay();
            return;
        }
        let ready = self.replay.take_ready(self.clock.now(), force);
        for batch in ready {
            self.send_replay(batch, deadline);
        }
    }
    fn send_replay(&mut self, mut batch: Batch, deadline: Option<Instant>) {
        if deadline.is_some_and(|end| self.clock.now() >= end) {
            self.report_replay_shutdown(None);
            return;
        }
        let length = batch.frames.iter().try_fold(1usize, |n, frame| {
            n.checked_add(frame.bytes.len())?.checked_add(1)
        });
        if length.is_none() || length.is_some_and(|n| n > MAX_BODY_BYTES) {
            if batch.frames.len() > 1 {
                self.split_replay(batch, deadline);
            } else {
                report(
                    &self.options,
                    "Screenshot request exceeds its byte limit",
                    None,
                );
            }
            return;
        }
        let mut body = Vec::with_capacity(length.unwrap());
        body.push(b'[');
        for (i, frame) in batch.frames.iter().enumerate() {
            if i != 0 {
                body.push(b',');
            }
            body.extend_from_slice(&frame.bytes);
        }
        body.push(b']');
        let compressed = self.options.capture_compression.is_some();
        let body = if compressed {
            let mut gzip = GzEncoder::new(
                LimitedBuffer {
                    bytes: Vec::new(),
                    limit: MAX_BODY_BYTES as u64,
                },
                Compression::default(),
            );
            gzip.write_all(&body)
                .and_then(|()| gzip.finish())
                .map(|body| body.bytes)
        } else {
            Ok(body)
        };
        let Ok(body) = body else {
            report(
                &self.options,
                "Screenshot compression exceeded its byte or encoding limit",
                None,
            );
            return;
        };
        let timeout = Duration::from_secs(self.options.request_timeout_seconds);
        let timeout = deadline.map_or(timeout, |end| {
            timeout.min(end.saturating_duration_since(self.clock.now()))
        });
        if timeout.is_zero() {
            return;
        }
        let request = self
            .http
            .post(format!(
                "{}{}",
                self.options.endpoints().base_host(),
                batch.frames[0].attribution.endpoint
            ))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::USER_AGENT,
                crate::client::get_default_user_agent(),
            )
            .timeout(timeout)
            .body(body);
        let request = if compressed {
            request.header(reqwest::header::CONTENT_ENCODING, "gzip")
        } else {
            request
        };
        let result = request.send();
        let mut retry_after = None;
        let status = match result {
            Err(_) => None,
            Ok(response) => {
                let status = response.status().as_u16();
                retry_after = parse_retry_after(response.headers());
                let mut bytes = Vec::new();
                if response.take(4097).read_to_end(&mut bytes).is_err() {
                    // A failed optional body cannot turn a known rejection into
                    // an unknown transport failure. Successful bodies may carry quota state.
                    (!(200..=299).contains(&status)).then_some(status)
                } else {
                    let quota = serde_json::from_slice::<serde_json::Value>(&bytes)
                        .ok()
                        .is_some_and(|v| {
                            ["quotaLimited", "quota_limited"].iter().any(|key| {
                                v.get(key).is_some_and(|q| {
                                    q.as_bool() == Some(true)
                                        || q.as_array().is_some_and(|values| {
                                            values.iter().any(|v| v.as_str() == Some("recordings"))
                                        })
                                })
                            })
                        });
                    if quota {
                        report(
                            &self.options,
                            "Screenshot upload rejected by recording quota",
                            Some(status),
                        );
                        return;
                    }
                    Some(status)
                }
            }
        };
        match status {
            Some(200..=299) => {}
            Some(413) if batch.frames.len() > 1 => self.split_replay(batch, deadline),
            Some(status) if !is_retryable_status(status) => report(
                &self.options,
                "Screenshot upload permanently rejected",
                Some(status),
            ),
            _ if deadline.is_some() => self.report_replay_shutdown(status),
            _ => {
                batch.attempt = batch.attempt.saturating_add(1);
                // Unlike the iOS and Android queues, which keep frames on disk,
                // this lane holds them only in memory: the attempt limit keeps a
                // failing upload from holding frame capacity and pausing
                // recording forever.
                if batch.attempt >= MAX_ATTEMPTS {
                    report(
                        &self.options,
                        "Screenshot upload failed after the maximum attempts",
                        status,
                    );
                    return;
                }
                let delay = capture_backoff_duration(&self.options, batch.attempt, retry_after);
                batch.due = self.clock.now() + delay;
                tracing::debug!(
                    status,
                    attempt = batch.attempt,
                    retained_frames = batch.frames.len(),
                    retry_delay_ms = delay.as_millis(),
                    "posthog-rs: retrying screenshot upload"
                );
                self.replay.retries.push_back(batch);
            }
        }
    }
    fn split_replay(&mut self, batch: Batch, deadline: Option<Instant>) {
        for frame in batch.frames {
            self.send_replay(
                Batch {
                    frames: vec![frame],
                    attempt: batch.attempt,
                    due: batch.due,
                },
                deadline,
            );
        }
    }
}

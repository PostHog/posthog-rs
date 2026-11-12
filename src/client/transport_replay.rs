//! Native screenshot delivery on the existing worker, independently of v1 verdicts.
use flate2::{write::GzEncoder, Compression};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use super::{ClientOptions, Pipeline};
use crate::client::common::apply_on_error_hooks;
use crate::client::event_store::RecordBuffer;
use crate::client::on_error::ReplayFailure;
use crate::client::retry::{capture_backoff_duration, is_retryable_status, parse_retry_after};
use crate::client::stateful::replay::EncodedFrame;
use crate::client::PostHogError;

pub(crate) const MAX_BODY_BYTES: usize = 25 * 1024 * 1024;
const INTERVAL: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct Queue {
    waiting: Vec<EncodedFrame>,
    since: Option<Instant>,
    retries: VecDeque<Batch>,
    closing: Option<Instant>,
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
    pub(super) fn next_due(&self) -> Option<Instant> {
        self.since
            .map(|since| since + INTERVAL)
            .into_iter()
            .chain(self.retries.iter().map(|b| b.due))
            .chain(
                self.closing
                    .filter(|_| !self.waiting.is_empty() || !self.retries.is_empty()),
            )
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

impl Pipeline {
    pub(super) fn admit_replay(&mut self, frame: EncodedFrame) {
        self.replay.push(frame, self.clock.now());
    }
    pub(super) fn close_replay(&mut self, deadline: Instant) {
        self.replay.closing = Some(
            self.replay
                .closing
                .map_or(deadline, |old| old.min(deadline)),
        );
        self.service_replay(true, Some(deadline));
    }
    pub(super) fn service_replay(&mut self, force: bool, deadline: Option<Instant>) {
        let deadline = deadline.into_iter().chain(self.replay.closing).min();
        if deadline.is_some_and(|end| self.clock.now() >= end) {
            self.replay.waiting.clear();
            self.replay.retries.clear();
            self.replay.since = None;
            return;
        }
        let ready = self.replay.take_ready(self.clock.now(), force);
        for batch in ready {
            self.send_replay(batch, deadline);
        }
    }
    fn send_replay(&mut self, mut batch: Batch, deadline: Option<Instant>) {
        if deadline.is_some_and(|end| self.clock.now() >= end) {
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
        let mut gzip = GzEncoder::new(
            RecordBuffer {
                bytes: Vec::new(),
                limit: MAX_BODY_BYTES as u64,
            },
            Compression::default(),
        );
        let body = gzip
            .write_all(&body)
            .and_then(|()| gzip.finish())
            .map(|body| body.bytes);
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
        let result = self
            .http
            .post(format!("{}/s/", self.options.endpoints().base_host()))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::CONTENT_ENCODING, "gzip")
            .header(
                reqwest::header::USER_AGENT,
                crate::client::get_default_user_agent(),
            )
            .timeout(timeout)
            .body(body)
            .send();
        let mut retry_after = None;
        let status = match result {
            Err(_) => None,
            Ok(response) => {
                let status = response.status().as_u16();
                retry_after = parse_retry_after(response.headers());
                let mut bytes = Vec::new();
                if response.take(4097).read_to_end(&mut bytes).is_err() {
                    None
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
            _ if deadline.is_some() => {}
            _ => {
                batch.attempt = batch.attempt.saturating_add(1);
                batch.due = self.clock.now()
                    + capture_backoff_duration(&self.options, batch.attempt, retry_after);
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

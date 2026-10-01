//! The `on_error` observability hook and the failure types it surfaces.
//!
//! Registering a hook via [`ClientOptionsBuilder::on_error`] lets a caller
//! observe terminal failures across the SDK's network surfaces — capture batch
//! delivery, remote `/flags` requests, and the local-evaluation definitions
//! poller — without reverting to a blocking API. The hook receives a
//! [`PostHogError`], a `#[non_exhaustive]` enum with one variant per surface so
//! more can be added without breaking callers.
//!
//! # The hook is observability-only — never emit from it
//!
//! A hook MUST NOT call back into the SDK: do not `capture`/`capture_batch`/
//! `capture_exception`, and do not `flush`/`shutdown`. Emitting an event from
//! the hook while handling a *capture* failure forms an amplification loop — a
//! transport incident that drops one batch would have the hook enqueue more
//! events, which fail, which fire the hook again. Because the hook is `Fn +
//! Send + Sync` and invoked without holding any SDK lock, it may run
//! concurrently on multiple threads and must be internally thread-safe. Filter
//! for the signal you care about and surface it (log, counter, channel) —
//! nothing more.
//!
//! [`ClientOptionsBuilder::on_error`]: crate::ClientOptionsBuilder::on_error

use std::sync::Arc;

use crate::error::Error;

use std::collections::HashMap;
use uuid::Uuid;

use crate::capture_event::{CaptureErrorResponse, EventResult, EventStatus};
use crate::endpoints::Endpoint;

type OnErrorFn = dyn Fn(&PostHogError<'_>) + Send + Sync + 'static;
type SharedOnErrorHook = Arc<OnErrorFn>;

/// A registered `on_error` hook.
///
/// Crate-internal: callers register hooks through
/// [`ClientOptionsBuilder::on_error`](crate::ClientOptionsBuilder::on_error)
/// and never name this type. Cloning shares the same underlying closure (it is
/// `Arc`-backed), so a hook can be invoked from whichever thread reaches a
/// failure (the transport worker, a flags request, or the poller).
#[derive(Clone)]
pub(crate) struct OnErrorHook(SharedOnErrorHook);

impl OnErrorHook {
    pub(crate) fn new<F>(hook: F) -> Self
    where
        F: Fn(&PostHogError<'_>) + Send + Sync + 'static,
    {
        Self(Arc::new(hook))
    }

    /// Invoke the hook.
    pub(crate) fn apply(&self, failure: &PostHogError<'_>) {
        (self.0)(failure)
    }
}

/// A terminal failure on one of the SDK's network surfaces, passed by reference
/// to each registered `on_error` hook.
///
/// `#[non_exhaustive]`: new variants may be added as more surfaces gain hook
/// coverage, so a `match` must include a wildcard arm.
#[derive(Debug)]
#[non_exhaustive]
pub enum PostHogError<'a> {
    /// A capture batch the SDK gave up delivering (permanent reject, exhausted
    /// retries, serialization failure) or — on the V1 pipeline — a `2xx` whose
    /// per-event verdicts left events unpersisted after the retry budget.
    Capture(CaptureFailure<'a>),
    /// A remote `/flags` request that failed (transport error, non-success
    /// status, or an unparseable response body).
    FeatureFlags(FlagsFailure<'a>),
    /// A background local-evaluation definitions poll that failed (transport
    /// error, non-success status, or an unparseable response body). The SDK
    /// keeps serving the previously cached definitions.
    LocalEvaluation(LocalEvaluationFailure<'a>),
}

/// Details of a terminal capture batch failure.
///
/// Fields are read through accessors; the struct is `#[non_exhaustive]`.
///
/// Does not fire for shutdown-timeout, queue-full, `before_send`, or local
/// oversize-AI-event drops — those are not delivery failures.
#[derive(Debug)]
#[non_exhaustive]
pub struct CaptureFailure<'a> {
    pub(crate) endpoint: Endpoint,
    pub(crate) error: Option<&'a Error>,
    pub(crate) status: Option<u16>,
    pub(crate) attempt: u32,
    pub(crate) event_count: usize,
    pub(crate) historical_migration: bool,
    pub(crate) request_id: Option<&'a Uuid>,
    pub(crate) results: &'a HashMap<Uuid, EventResult>,
    pub(crate) error_response: Option<&'a CaptureErrorResponse>,
}

impl<'a> CaptureFailure<'a> {
    /// The capture endpoint the failed batch targeted: [`Endpoint::Capture`]
    /// for `capture`/`capture_batch`, [`Endpoint::CaptureAi`] for
    /// `capture_ai`/`capture_ai_batch`. Lets one hook tell the lanes apart.
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// The batch-level cause: a permanent reject, exhausted transport/HTTP
    /// retries, or a serialization failure.
    #[doc = "\n`None` only when the request itself succeeded (`2xx`) but some events were not\npersisted after the retry budget — inspect [`event_results`](Self::event_results)."]
    pub fn error(&self) -> Option<&Error> {
        self.error
    }

    /// The HTTP status of the final attempt, or `None` when no response was
    /// received (a transport error or a serialization failure before sending).
    pub fn status(&self) -> Option<u16> {
        self.status
    }

    /// The failing attempt number (equals the configured maximum on exhaustion).
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Number of events this failure dropped (lost).
    #[doc = "\nCounts only undelivered events (`retry`/`drop`), including any finalized on\nearlier attempts. This can be smaller than [`event_results`](Self::event_results)`.len()`,\nwhich also reports persisted `ok`/`warning` verdicts — filter by status before\ntreating an entry as lost."]
    pub fn event_count(&self) -> usize {
        self.event_count
    }

    /// Whether the batch was a historical-migration batch.
    pub fn historical_migration(&self) -> bool {
        self.historical_migration
    }

    /// The capture `posthog-request-id` of the final attempt, when one was
    /// sent. `None` for a serialization failure where no request reached the
    /// wire.
    pub fn request_id(&self) -> Option<&Uuid> {
        self.request_id
    }

    /// Per-event server verdicts for the capture batch.
    ///
    /// Maps event UUID to its [`EventResult`]. Includes **all** verdicts the
    /// batch collected — persisted (`ok`/`warning`) as well as lost
    /// (`retry`/`drop`) — so this map can be larger than
    /// [`event_count`](Self::event_count); filter by
    /// [`EventStatus`](crate::EventStatus) to isolate the failures. Complete
    /// when [`error`](Self::error) is `None` (a `2xx` where events weren't
    /// persisted after retries); possibly partial on a batch-level failure
    /// (only verdicts collected from earlier attempts).
    pub fn event_results(&self) -> &HashMap<Uuid, EventResult> {
        self.results
    }

    /// One line with every `drop` and `retry` reason in the batch and its count, drops first
    /// and most frequent first, for example `drop/invalid_options=2, retry/not_persisted=1`,
    /// or an empty string when nothing was lost.
    ///
    /// Reasons come from the server, so the summary strips control characters and line
    /// separators, cuts each reason to 64 characters, and shows a missing one as `unspecified`.
    ///
    /// ```no_run
    /// use posthog_rs::{ClientOptionsBuilder, PostHogError};
    ///
    /// let options = ClientOptionsBuilder::default()
    ///     .api_key("phc_example".to_string())
    ///     .on_error(|failure| {
    ///         if let PostHogError::Capture(capture) = failure {
    ///             eprintln!(
    ///                 "{} event(s) lost: {}",
    ///                 capture.event_count(),
    ///                 capture.verdict_summary()
    ///             );
    ///         }
    ///     })
    ///     .build()
    ///     .unwrap();
    /// ```
    pub fn verdict_summary(&self) -> String {
        verdict_summary(self.results)
    }

    /// The structured error body returned by the capture backend on a
    /// non-`2xx` response (`error`, `error_description`, `error_uri`), when the
    /// body parsed as one. `None` for a transport error, a `2xx`, or an
    /// unrecognizable body — the raw body remains available via
    /// [`error`](Self::error).
    pub fn error_response(&self) -> Option<&CaptureErrorResponse> {
        self.error_response
    }
}

/// Longest server reason a verdict summary shows, in characters.
const MAX_SUMMARY_REASON_CHARS: usize = 64;

fn verdict_summary(results: &HashMap<Uuid, EventResult>) -> String {
    let mut entries = Vec::new();
    for (status, label) in [(EventStatus::Drop, "drop"), (EventStatus::Retry, "retry")] {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for r in results.values().filter(|r| r.result == status) {
            *counts
                .entry(summary_reason(r.details.as_deref()))
                .or_insert(0) += 1;
        }
        let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        entries.extend(
            ranked
                .into_iter()
                .map(|(reason, n)| format!("{label}/{reason}={n}")),
        );
    }
    entries.join(", ")
}

/// Clip a server reason and strip characters that would split the log line it lands in.
fn summary_reason(details: Option<&str>) -> String {
    let reason: String = details
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}'))
        .take(MAX_SUMMARY_REASON_CHARS)
        .collect();
    if reason.trim().is_empty() {
        "unspecified".to_string()
    } else {
        reason
    }
}

/// Details of a failed remote `/flags` request.
///
/// Fields are read through accessors; the struct is `#[non_exhaustive]`.
#[derive(Debug)]
#[non_exhaustive]
pub struct FlagsFailure<'a> {
    pub(crate) error: &'a Error,
    pub(crate) endpoint: &'a str,
    pub(crate) distinct_id: Option<&'a str>,
    pub(crate) status: Option<u16>,
    pub(crate) body: Option<&'a str>,
}

impl<'a> FlagsFailure<'a> {
    /// The cause of the failure ([`Error::Connection`] for transport or
    /// non-success status, [`Error::Serialization`] for an unparseable body).
    pub fn error(&self) -> &Error {
        self.error
    }

    /// The `/flags` endpoint URL the request targeted.
    pub fn endpoint(&self) -> &str {
        self.endpoint
    }

    /// The `distinct_id` the request was evaluating flags for, when known.
    pub fn distinct_id(&self) -> Option<&str> {
        self.distinct_id
    }

    /// The HTTP status, or `None` when no response was received (a transport
    /// error or an exhausted transient-retry budget).
    pub fn status(&self) -> Option<u16> {
        self.status
    }

    /// The response body, when one was read (present on a non-success status).
    pub fn body(&self) -> Option<&str> {
        self.body
    }
}

/// Details of a failed local-evaluation definitions poll.
///
/// Fields are read through accessors; the struct is `#[non_exhaustive]`.
/// Credentials (the personal API key) are never surfaced here.
#[derive(Debug)]
#[non_exhaustive]
pub struct LocalEvaluationFailure<'a> {
    pub(crate) error: &'a Error,
    pub(crate) status: Option<u16>,
}

impl<'a> LocalEvaluationFailure<'a> {
    /// The cause of the failure ([`Error::Connection`] for transport or
    /// non-success status, [`Error::Serialization`] for an unparseable body).
    pub fn error(&self) -> &Error {
        self.error
    }

    /// The HTTP status, or `None` when no response was received (a transport
    /// error).
    pub fn status(&self) -> Option<u16> {
        self.status
    }
}

#[cfg(test)]
pub(crate) fn verdicts(entries: &[(EventStatus, Option<&str>)]) -> HashMap<Uuid, EventResult> {
    entries
        .iter()
        .map(|(result, details)| {
            (
                Uuid::now_v7(),
                EventResult {
                    result: result.clone(),
                    details: details.map(str::to_string),
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use EventStatus::{Drop, Ok, Retry, Warning};

    #[test]
    fn verdict_summary_lists_every_lost_reason_in_order() {
        let mut entries: Vec<(EventStatus, Option<&str>)> = Vec::new();
        for (reason, n) in [
            ("r1", 1),
            ("r2", 3),
            ("r3", 1),
            ("r4", 2),
            ("r5", 1),
            ("r6", 1),
        ] {
            entries.extend(std::iter::repeat((Drop, Some(reason))).take(n));
        }
        entries.extend([
            (Retry, Some("not_persisted")),
            (Ok, None),
            (Warning, Some("person_processing_disabled")),
        ]);
        assert_eq!(
            verdict_summary(&verdicts(&entries)),
            "drop/r2=3, drop/r4=2, drop/r1=1, drop/r3=1, drop/r5=1, drop/r6=1, retry/not_persisted=1"
        );
    }

    #[test]
    fn verdict_summary_is_empty_when_nothing_was_lost() {
        assert_eq!(
            verdict_summary(&verdicts(&[(Ok, None), (Warning, None)])),
            ""
        );
    }

    #[test]
    fn verdict_summary_sanitizes_server_reasons() {
        let long = "x".repeat(100);
        let results = verdicts(&[
            (Drop, Some(long.as_str())),
            (Drop, Some("bad\nreason")),
            (Drop, Some("split\u{2028}line\u{2029}")),
            (Drop, None),
            (Retry, Some(" ")),
        ]);
        assert_eq!(
            verdict_summary(&results),
            format!(
                "drop/badreason=1, drop/splitline=1, drop/unspecified=1, drop/{}=1, retry/unspecified=1",
                "x".repeat(64)
            )
        );
    }
}

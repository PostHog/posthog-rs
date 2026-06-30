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
//! events, which fail, which fire the hook again. The hook also runs while the
//! SDK holds the hook's own mutex, so re-entering the SDK in a way that fires
//! `on_error` again on the same thread would deadlock. Filter for the signal
//! you care about and surface it (log, counter, channel) — nothing more.
//!
//! [`ClientOptionsBuilder::on_error`]: crate::ClientOptionsBuilder::on_error

use std::sync::{Arc, Mutex};

use crate::error::Error;

#[cfg(feature = "capture-v1")]
use std::collections::HashMap;
#[cfg(feature = "capture-v1")]
use uuid::Uuid;

#[cfg(feature = "capture-v1")]
use crate::event_v1::{EventResult, V1ErrorResponse};

type OnErrorFn = dyn FnMut(&PostHogError<'_>) + Send + 'static;
type SharedOnErrorHook = Arc<Mutex<Box<OnErrorFn>>>;

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
        F: FnMut(&PostHogError<'_>) + Send + 'static,
    {
        Self(Arc::new(Mutex::new(Box::new(hook))))
    }

    /// Invoke the hook. Recovers a poisoned mutex (a prior hook panic) so one
    /// bad invocation doesn't permanently wedge the hook.
    pub(crate) fn apply(&self, failure: &PostHogError<'_>) {
        let mut hook = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (hook)(failure)
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
/// Does not fire for shutdown-timeout, queue-full, or `before_send` drops —
/// those are not delivery failures.
#[derive(Debug)]
#[non_exhaustive]
pub struct CaptureFailure<'a> {
    pub(crate) error: Option<&'a Error>,
    pub(crate) status: Option<u16>,
    pub(crate) attempt: u32,
    pub(crate) event_count: usize,
    pub(crate) historical_migration: bool,
    #[cfg(feature = "capture-v1")]
    pub(crate) request_id: Option<&'a Uuid>,
    #[cfg(feature = "capture-v1")]
    pub(crate) results: &'a HashMap<Uuid, EventResult>,
    #[cfg(feature = "capture-v1")]
    pub(crate) error_response: Option<&'a V1ErrorResponse>,
}

impl<'a> CaptureFailure<'a> {
    /// The batch-level cause: a permanent reject, exhausted transport/HTTP
    /// retries, or a serialization failure.
    #[cfg_attr(
        not(feature = "capture-v1"),
        doc = "\nAlways present: every capture failure surfaced to the hook carries a cause."
    )]
    #[cfg_attr(
        feature = "capture-v1",
        doc = "\n`None` only when the request itself succeeded (`2xx`) but some events were not\npersisted after the retry budget — inspect [`event_results`](Self::event_results)."
    )]
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
    #[cfg_attr(
        feature = "capture-v1",
        doc = "\nCounts only undelivered events (`retry`/`drop`), including any finalized on\nearlier attempts. This can be smaller than [`event_results`](Self::event_results)`.len()`,\nwhich also reports persisted `ok`/`warning` verdicts — filter by status before\ntreating an entry as lost."
    )]
    pub fn event_count(&self) -> usize {
        self.event_count
    }

    /// Whether the batch was a historical-migration batch.
    pub fn historical_migration(&self) -> bool {
        self.historical_migration
    }

    /// The V1 capture `posthog-request-id` of the final attempt, when one was
    /// sent. `None` for a serialization failure (no request reached the wire)
    /// and on the v0 pipeline (which has no request id).
    #[cfg(feature = "capture-v1")]
    pub fn request_id(&self) -> Option<&Uuid> {
        self.request_id
    }

    /// Per-event server verdicts for the batch (V1 capture pipeline only).
    ///
    /// Maps event UUID to its [`EventResult`]. Includes **all** verdicts the
    /// batch collected — persisted (`ok`/`warning`) as well as lost
    /// (`retry`/`drop`) — so this map can be larger than
    /// [`event_count`](Self::event_count); filter by
    /// [`EventStatus`](crate::EventStatus) to isolate the failures. Complete
    /// when [`error`](Self::error) is `None` (a `2xx` where events weren't
    /// persisted after retries); possibly partial on a batch-level failure
    /// (only verdicts collected from earlier attempts).
    #[cfg(feature = "capture-v1")]
    pub fn event_results(&self) -> &HashMap<Uuid, EventResult> {
        self.results
    }

    /// The structured error body returned by the V1 capture backend on a
    /// non-`2xx` response (`error`, `error_description`, `error_uri`), when the
    /// body parsed as one. `None` for a transport error, a `2xx`, or an
    /// unrecognizable body — the raw body remains available via
    /// [`error`](Self::error).
    #[cfg(feature = "capture-v1")]
    pub fn error_response(&self) -> Option<&V1ErrorResponse> {
        self.error_response
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

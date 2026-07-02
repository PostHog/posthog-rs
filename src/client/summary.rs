//! Outcome of a confirmed-delivery capture call.

#[cfg(feature = "capture-v1")]
use std::collections::HashMap;

#[cfg(feature = "capture-v1")]
use uuid::Uuid;

#[cfg(feature = "capture-v1")]
use crate::event_v1::{EventResult, EventStatus};

/// The outcome of a confirmed capture ([`Client::capture_confirmed`] /
/// [`Client::capture_batch_confirmed`]), returned once the SDK has a terminal
/// result for the batch — the request succeeded, or the retry budget was spent
/// (which is an [`Err`] instead).
///
/// A returned `CaptureSummary` means the capture request itself succeeded (HTTP
/// `2xx`). On the `capture-v1` pipeline the backend reports a per-event verdict,
/// so a `2xx` can still leave some events unpersisted (`drop`/`retry`) — check
/// [`all_persisted`](Self::all_persisted) / [`not_persisted`](Self::not_persisted)
/// before treating the batch as fully durable. On the v0 pipeline a `2xx`
/// persists the whole batch, so `all_persisted()` is always `true`.
///
/// `#[non_exhaustive]`: fields are read through accessors so more can be added
/// without breaking callers.
///
/// [`Client::capture_confirmed`]: crate::Client::capture_confirmed
/// [`Client::capture_batch_confirmed`]: crate::Client::capture_batch_confirmed
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CaptureSummary {
    submitted: usize,
    #[cfg(feature = "capture-v1")]
    results: HashMap<Uuid, EventResult>,
}

impl CaptureSummary {
    /// V1 outcome: the number of events sent plus the backend's per-event verdicts.
    #[cfg(feature = "capture-v1")]
    pub(crate) fn from_results(submitted: usize, results: HashMap<Uuid, EventResult>) -> Self {
        Self { submitted, results }
    }

    /// V0 outcome: a `2xx` persists the whole batch (no per-event verdicts).
    #[cfg(not(feature = "capture-v1"))]
    pub(crate) fn delivered(submitted: usize) -> Self {
        Self { submitted }
    }

    /// Number of events sent on the wire (after `before_send` filtering).
    pub fn submitted(&self) -> usize {
        self.submitted
    }

    /// Number of submitted events the backend did not persist.
    ///
    /// Always `0` on the v0 pipeline (a `2xx` persists the whole batch). On
    /// `capture-v1` this is `submitted` minus the events with an `ok`/`warning`
    /// verdict, so it counts both `drop`/`retry` verdicts and any submitted
    /// event the backend omitted from its response.
    pub fn not_persisted(&self) -> usize {
        #[cfg(feature = "capture-v1")]
        {
            let persisted = self
                .results
                .values()
                .filter(|r| matches!(r.result, EventStatus::Ok | EventStatus::Warning))
                .count();
            self.submitted.saturating_sub(persisted)
        }
        #[cfg(not(feature = "capture-v1"))]
        {
            0
        }
    }

    /// Whether every submitted event was persisted (`not_persisted() == 0`).
    pub fn all_persisted(&self) -> bool {
        self.not_persisted() == 0
    }

    /// Per-event server verdicts (`capture-v1` only). Includes persisted
    /// (`ok`/`warning`) and unpersisted (`drop`/`retry`) verdicts; filter by
    /// [`EventStatus`](crate::EventStatus) to isolate failures. May omit events
    /// the backend did not report on — see [`not_persisted`](Self::not_persisted).
    #[cfg(feature = "capture-v1")]
    pub fn event_results(&self) -> &HashMap<Uuid, EventResult> {
        &self.results
    }
}

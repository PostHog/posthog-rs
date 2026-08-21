//! Outcome of an immediate-delivery capture call.

use std::collections::HashMap;

use uuid::Uuid;

use crate::capture_event::{EventResult, EventStatus};

/// The outcome of an immediate capture ([`Client::capture_immediate`] /
/// [`Client::capture_batch_immediate`]), returned once the SDK has a terminal
/// result for the batch — the request succeeded, or the retry budget was spent
/// (which is an [`Err`] instead).
///
/// A returned `CaptureSummary` means the capture request itself succeeded (HTTP
/// `2xx`). The backend reports a per-event verdict, so a `2xx` can still leave
/// some events unpersisted (`drop`/`retry`) — check
/// [`all_persisted`](Self::all_persisted) / [`not_persisted`](Self::not_persisted)
/// before treating the batch as fully durable.
///
/// `#[non_exhaustive]`: fields are read through accessors so more can be added
/// without breaking callers.
///
/// [`Client::capture_immediate`]: crate::Client::capture_immediate
/// [`Client::capture_batch_immediate`]: crate::Client::capture_batch_immediate
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CaptureSummary {
    submitted: usize,
    results: HashMap<Uuid, EventResult>,
}

impl CaptureSummary {
    /// The number of events sent plus the backend's per-event verdicts.
    pub(crate) fn from_results(submitted: usize, results: HashMap<Uuid, EventResult>) -> Self {
        Self { submitted, results }
    }

    /// Number of events sent on the wire (after `before_send` filtering).
    pub fn submitted(&self) -> usize {
        self.submitted
    }

    /// Number of submitted events the backend did not persist.
    ///
    /// `submitted` minus the events with an `ok`/`warning` verdict, so it counts
    /// both `drop`/`retry` verdicts and any submitted event the backend omitted
    /// from its response.
    pub fn not_persisted(&self) -> usize {
        let persisted = self
            .results
            .values()
            .filter(|r| matches!(r.result, EventStatus::Ok | EventStatus::Warning))
            .count();
        self.submitted.saturating_sub(persisted)
    }

    /// Whether every submitted event was persisted (`not_persisted() == 0`).
    ///
    /// Note this is **vacuously true when nothing was sent** (`submitted() == 0`),
    /// which is what a disabled client or a fully `before_send`-filtered batch
    /// returns. Callers that advance durable state on the strength of an immediate
    /// delivery (e.g. committing an upstream offset) must therefore also check
    /// `submitted()` against the number of events they intended to send — do not
    /// gate durability on `all_persisted()` alone.
    pub fn all_persisted(&self) -> bool {
        self.not_persisted() == 0
    }

    /// Per-event server verdicts. Includes persisted
    /// (`ok`/`warning`) and unpersisted (`drop`/`retry`) verdicts; filter by
    /// [`EventStatus`](crate::EventStatus) to isolate failures. May omit events
    /// the backend did not report on — see [`not_persisted`](Self::not_persisted).
    pub fn event_results(&self) -> &HashMap<Uuid, EventResult> {
        &self.results
    }
}

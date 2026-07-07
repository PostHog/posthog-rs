---
cargo/posthog-rs: minor
---

Immediate (inline) capture for callers that must know a batch persisted before advancing their own durable state (e.g. a server-side importer committing an upstream offset). `Client::capture_immediate` and `Client::capture_batch_immediate` send inline — bypassing the background transport worker — and return `Result<CaptureSummary, Error>` once the request reaches a terminal outcome, retrying transient failures per the client's existing retry configuration. They are available on both the async and blocking clients and on both the v0 and `capture-v1` pipelines, reusing the same sans-IO request-build/retry/response-classification core as the fire-and-forget path.

The new public `CaptureSummary` (`#[non_exhaustive]`, read through accessors) reports the outcome: `submitted()` (events sent after `before_send` filtering), `not_persisted()`, and `all_persisted()`. On the v0 pipeline a `2xx` persists the whole batch, so `all_persisted()` is always true; on `capture-v1` the backend returns per-event verdicts, so a `2xx` can still leave events unpersisted — `not_persisted()` counts submitted events without an `ok`/`warning` verdict, and `event_results()` (under `capture-v1`) exposes the full per-event map.

Fire-and-forget `capture`/`capture_batch` remain the primary, recommended API and are unchanged. The immediate variants do NOT fire `on_error` hooks: the returned `Result` is the delivery signal, so there is no double-reporting. Disabled clients and an empty (or fully `before_send`-filtered) batch return a default `CaptureSummary` without sending a request. New public export: `CaptureSummary`.

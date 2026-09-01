---
cargo/posthog: major
---

Remove the legacy V0 capture pipeline. Capture V1 (`POST /i/v1/analytics/events`) is now the only capture implementation and the `capture-v1` Cargo feature has been removed.

Breaking changes:

- `V1ErrorResponse` is renamed `CaptureErrorResponse`, which also changes the return type of `CaptureFailure::error_response()`.
- `Endpoint::Batch` is removed, and `Endpoint` is now `#[non_exhaustive]` so future endpoints can be added without breaking callers.
- `Endpoint::Capture` now resolves to `/i/v1/analytics/events` instead of `/i/v0/e/`. Code calling `Endpoint::Capture.path()` compiles unchanged but observes a different URL.
- The `capture-v1` feature no longer exists; remove it from your feature list. `brotli` and `zstd` are now unconditional dependencies (`zstd-sys` builds C via `cc`).

Behavior changes:

- All four content encodings the capture endpoint accepts — `gzip`, `deflate`, `br`, `zstd` — now work. Previously, selecting `CaptureCompression::{Deflate, Br, Zstd}` on default features silently sent the body **uncompressed**; it is now genuinely compressed. Check any proxy or WAF in front of PostHog that may not handle `Content-Encoding: zstd` or `br`.
- `CaptureSummary::not_persisted()` and `all_persisted()` are now data-driven from the backend's per-event verdicts. On V0 they were hard-wired to `0` and `true`, so callers gating durable state on `all_persisted()` should re-check that logic.
- HTTP 429 is no longer specially retried. The V1 endpoint does not emit 429 (it uses 402 for billing limits and per-event `drop`/`warning` verdicts on a 200); `Retry-After` continues to be honored on 408/500/503 and on 200 responses carrying `retry` verdicts.

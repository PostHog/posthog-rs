---
cargo/posthog-rs: minor
---

Add `CaptureFailure::verdict_summary()`, which lists every dropped and retried event reason with its count on one line, for logging from an `on_error` hook. Without a hook, the per-batch warning now shows totals only, for example `3 event(s) not persisted by <endpoint>: 2 dropped, 1 out of retries`.

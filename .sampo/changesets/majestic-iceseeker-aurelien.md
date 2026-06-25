---
cargo/posthog-rs: minor
---

Add an `on_error` client hook to observe terminal batch delivery failures (permanent rejects and exhausted retries). The background worker invokes registered hooks with a `CaptureFailure` (cause, event count, attempt); registering one also silences the default drop WARN.

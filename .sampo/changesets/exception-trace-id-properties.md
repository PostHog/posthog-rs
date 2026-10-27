---
cargo/posthog-rs: minor
---

A `$trace_id` or `$span_id` set with `CaptureExceptionOptions::property` now wins over the trace context provider, and setting either one stops the SDK adding both provider IDs. `CaptureExceptionOptions::trace_context` still wins over both.

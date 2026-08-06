---
cargo/posthog-rs: minor
---

Add `trace_id` and `span_id` capture options so exception events can be linked to distributed traces. When the optional `opentelemetry` feature is enabled, exception capture automatically reads both IDs from the current OpenTelemetry span.

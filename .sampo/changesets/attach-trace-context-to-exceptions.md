---
cargo/posthog-rs: minor
---

Add an atomic `trace_context` capture option and a client-level trace-context provider so exception events can be linked to distributed traces without adding telemetry dependencies to the SDK.

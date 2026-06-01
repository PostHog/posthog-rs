---
cargo/posthog-rs: minor
---

Add V1 capture pipeline (`/i/v1/analytics/events/`) with gzip/deflate/br/zstd compression, automatic partial-batch retry with exponential backoff, per-event options (cookieless mode, skew correction, person profile, product tour), historical migration support, and SDK test harness integration with parallel test execution.

---
cargo/posthog-rs: minor
---

Add V1 capture pipeline (`/i/v1/analytics/events/`) behind the unstable `capture-v1` Cargo feature (off by default). Includes gzip/deflate/br/zstd compression, automatic partial-batch retry with exponential backoff, per-event options (cookieless mode, skew correction, person profile, product tour), and historical migration support. A separate `test-harness` feature enables injecting extra request headers for compliance test isolation.

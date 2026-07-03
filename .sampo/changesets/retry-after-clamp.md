---
cargo/posthog-rs: minor
---

Clamp a server `Retry-After` to `retry_max_backoff_ms` (default 30s) instead of an internal 1-day cap, so a single retry wait never exceeds the configured max backoff. `Retry-After` still acts as a minimum and the configured backoff itself is never truncated. This unifies the default retry-wait ceiling (30s) with posthog-go and posthog-python.

---
cargo/posthog-rs: minor
---

Retry capture requests that get HTTP 404 or 429, like 408 and 5xx, with the same backoff and `Retry-After` as the minimum wait. 0.x retried a 429 only when it had a `Retry-After` header and never retried a 404.

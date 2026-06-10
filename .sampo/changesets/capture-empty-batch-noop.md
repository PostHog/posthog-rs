---
cargo/posthog-rs: patch
---

`capture_batch` with an empty event list is now a no-op on both clients and
both capture paths — no HTTP request is sent (the v1 backend rejects empty
batches). Also derives `Debug` for the internal retry `Step` and pins that a
body-less 2xx response is a terminal serialization error, not a retry.

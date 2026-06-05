---
cargo/posthog-rs: patch
---

Serialize the v0 capture `distinct_id` at the event root (canonical field) instead of the legacy `$distinct_id` alias, matching v1 and the v0 ingestion contract. Add retries to the v0 capture paths: transport errors and 408/500/502/503/504 are retried with exponential backoff honoring `Retry-After`, and a 429 is retried only when it carries a `Retry-After` (a bare 429 stays a terminal rate-limit). Retried requests resend the same bytes, preserving the event UUID and timestamp that dedup relies on. Add opt-in gzip compression for v0 capture via `capture_compression`: the body is gzipped with a `Content-Encoding: gzip` header and a `compression=gzip` query param (capture reads the query param on v0).

Fix the retry backoff timing on both v0 and v1: the first retry now waits exactly `retry_initial_backoff_ms` instead of double it (the call sites previously passed `attempt + 1` into the backoff calculation, skipping the configured initial delay).

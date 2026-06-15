---
posthog-rs: minor
---

Runtime-independent background event transport. `capture` and `capture_batch` are now non-blocking enqueues onto a background worker — a plain `std::thread` with a blocking HTTP client, independent of any async runtime — that batches events, retries transient failures with backoff (honoring `Retry-After`), and sends them. They no longer block on the network or return delivery errors.

New public API: `flush()` (awaited on the async client, blocking on the blocking client), `shutdown()` (flush + stop the worker + join; idempotent; drops further captures), flush-on-`Drop`, and `pending_events()`. New `ClientOptions`: `flush_at`, `max_batch_size`, `flush_interval_ms`, and `max_queue_size` (a bounded queue that drops with a single warning when full). `before_send` hooks now run on the worker thread, so they apply to every queued event.

Breaking change (0.x): `capture`/`capture_batch` return `Ok(())` as soon as the event is queued instead of awaiting delivery, and HTTP failures surface as logged warnings rather than `Err`. Call `flush()` or `shutdown()` before process exit to ensure queued events are delivered.

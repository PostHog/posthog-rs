---
cargo/posthog-rs: minor
---

Add `capture_ai`, `capture_ai_batch`, `capture_ai_immediate`, `capture_ai_batch_immediate` and the global `posthog_rs::capture_ai` for LLM analytics events. They post to `/i/v1/ai/events` on their own background lane, with the `capture_ai_compression` and `capture_ai_max_queue_size` options. Both the background lane and the immediate variants drop an event over 8 MiB locally; the immediate variants report it in the returned `CaptureSummary` as an `ai_event_too_big` drop.

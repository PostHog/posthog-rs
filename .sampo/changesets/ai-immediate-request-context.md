---
cargo/posthog-rs: patch
---

`capture_ai_immediate` and `capture_ai_batch_immediate` now apply the current request context, like `capture_ai`, `capture_immediate` and `capture_batch_immediate`. Before, context properties (such as `$session_id`) and context options were missing from events sent this way.

---
cargo/posthog-rs: patch
---

Event timestamps keep microsecond and nanosecond precision instead of being cut to milliseconds. Whole-millisecond timestamps are sent as before.

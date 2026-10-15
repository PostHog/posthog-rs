---
cargo/posthog-rs: minor
---

Log a warning when the SDK drops a `$session_id` or `$window_id` value that is not a string. The warning names the key and the value's type, never the value. A `null` value still drops without a warning.

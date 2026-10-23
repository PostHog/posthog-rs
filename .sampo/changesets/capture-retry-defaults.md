---
cargo/posthog-rs: minor
---

Capture now makes 4 attempts by default instead of 3, and the first retry waits 100 ms instead of 200 ms. Remote `/flags` retries no longer use the capture retry options. They wait 300 ms before the first retry and double up to 30 s.

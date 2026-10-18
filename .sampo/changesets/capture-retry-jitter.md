---
cargo/posthog-rs: patch
---

Capture retry waits now vary at random by up to 25% either way, so many clients do not retry at the same moment.

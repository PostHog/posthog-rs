---
cargo/posthog-rs: patch
---

When a `before_send` hook panics, the error log now names the event it dropped: `panic in PostHog before_send hook for event "<name>"; dropping event`. It still never logs the panic message, which can hold the values the hook was redacting.

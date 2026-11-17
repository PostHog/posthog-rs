---
cargo/posthog: patch
---

When an MCP analytics `before_send` hook panics, the SDK now logs an error that names the event it dropped: `panic in PostHog before_send hook for event "<name>"; dropping event`. Before, it logged a warning without the event name. It never logs the panic message.

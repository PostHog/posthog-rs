---
cargo/posthog-rs: patch
---

Omit null object properties recursively when serializing captured events, while preserving null array entries and generated feature flag response metadata.

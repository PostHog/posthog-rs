---
cargo/posthog-rs: minor
---

feat(feature-flags): support `early_exit` in local evaluation

Adds support for the `early_exit` option on a feature flag's `filters` during local evaluation. When `early_exit` is `true` and a condition group's property filters match (or it has no property filters) but the rollout percentage excludes the user, evaluation now stops and returns a definitive disabled result instead of falling through to later condition groups. A property-filter mismatch always falls through, regardless of `early_exit`. The flag defaults to `false` (absent), preserving the existing fall-through behavior. This mirrors the PostHog Rust evaluation engine and the posthog-node/posthog-python implementations.

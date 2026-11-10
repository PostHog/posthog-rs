---
cargo/posthog: minor
---

Add current-user feature flags to the stateful client: enabled-by-default asynchronous startup preload, bootstrap and persistent cached values, targeting overrides, cached boolean/variant results, load listeners, and consent-aware access reporting. Cached reads are synchronous; reloads use the shared delivery worker.

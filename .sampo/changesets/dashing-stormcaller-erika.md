---
cargo/posthog: minor
---

Add `stateful::Client` behind the optional `stateful` Cargo feature, with persisted anonymous identity, synchronous capture and screen events, native before-send hooks, and blocking or async initialization and drains. Persistence defaults to the OS application-data directory with directory and memory overrides; storage failures retain enabled memory-backed operation. Shutdown accepts a per-call best-effort duration, while in-flight work and final joins may exceed it.

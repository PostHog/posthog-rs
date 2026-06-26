---
cargo/posthog-rs: patch
---

Type-coerce `capture-v1` options before placing them on the wire. A caller value whose type doesn't match the backend's strict `Options` schema is now coerced when possible, or dropped (backend applies its default) rather than rejecting the whole batch. No effect on default (v0) capture behavior.

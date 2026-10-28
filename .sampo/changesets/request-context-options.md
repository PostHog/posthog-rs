---
cargo/posthog-rs: minor
---

`RequestContext::option` sets a capture option on every event in a request context, including exceptions. An option the event sets, other than `null`, wins.

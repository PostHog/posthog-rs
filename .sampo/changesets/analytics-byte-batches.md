---
cargo/posthog-rs: minor
---

Split each background analytics batch into requests of at most 5 MiB of serialized events, so a batch of large events no longer exceeds capture's 10 MiB request limit. A larger single event is sent on its own. `before_send` timing and `capture_batch_immediate` requests are unchanged.

---
cargo/posthog-rs: patch
---

`group_identify` now sends the request context's distinct id when the context has one, and `$<group_type>_<group_key>` otherwise. It no longer adds the context session id to `$groupidentify` events. This matches the other v1 server SDKs.

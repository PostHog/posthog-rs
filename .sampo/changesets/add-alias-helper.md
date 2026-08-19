---
cargo/posthog-rs: minor
---

Add a `Client::alias` helper that merges two distinct IDs onto the same person, so callers no longer need to hand-build the `$create_alias` event and know that `distinct_id` is written both at the top level and inside `properties`. Available on the async and blocking clients with the same fire-and-forget signature as `capture`. A blank ID on either side cannot describe a merge, so the event is dropped with a warning rather than sent.

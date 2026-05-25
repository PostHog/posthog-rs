---
cargo/posthog-rs: patch
---

Include group context in the `$feature_flag_called` dedupe key so group-scoped flags fire a separate event for each group a user is evaluated under, instead of being dedup-ed against the first group context the same `(distinct_id, flag, response)` was seen under.

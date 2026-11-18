---
cargo/posthog: patch
---

Local flag evaluation now checks condition groups in the order they are defined, like the other SDKs, instead of moving groups with a variant override to the front. For a flag with a variant override on a group other than the first, some users can now get a different value: the variant from the first group they match, or `false` from an `early_exit` group. With `early_exit`, a rollout exclusion now also stops evaluation after an earlier inconclusive group, so the flag stays inconclusive (and falls back to the server) instead of matching a later group.

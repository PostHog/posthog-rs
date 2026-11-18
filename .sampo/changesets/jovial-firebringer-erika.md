---
cargo/posthog: patch
---

Fix local flag evaluation for projects with an `is_set` or `is_not_set` filter that has no `value`.

PostHog accepts these two property filters without a `value`, but `Property` required one, so a single such filter made the whole `LocalEvaluationResponse` fail to deserialize and no flags loaded at all. They now deserialize without a `value`, on flags and on cohort leaves. Other operators still require one.

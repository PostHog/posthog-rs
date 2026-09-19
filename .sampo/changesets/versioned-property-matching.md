---
cargo/posthog-rs: minor
---

Honor definition snapshots' `property_matching_version` during local flag evaluation, including group conditions, recursive cohorts, and flag dependencies. Version 2 uses explicit case-insensitive equality instead of legacy boolean coercion; missing metadata and other versions retain legacy matching. Empty filters retain recursive truthiness in both versions. Keep matching semantics and definitions together across cache refreshes and version-only updates.

`LocalEvaluationResponse` and `EvaluationContext` struct literals now require a `property_matching_version` field (use `1` for legacy behavior); older serialized definitions still load without it. The context-free `match_feature_flag` helper remains legacy.

---
cargo/posthog: patch
---

Local evaluation of definitions loaded by `LocalEvaluator` now reads a condition's explicit `aggregation_group_type_index: null` as a person condition, as the PostHog flags service does. On a group flag without a usable group key, that condition still runs and can match, directly or as a dependency, instead of being skipped with the group conditions. A group flag with a holdout that excludes anyone, and no usable group key, is now inconclusive, so it falls back to `/flags` (or stays unresolved with `only_evaluate_locally`). The flags service would hash an empty identifier for that holdout.

---
cargo/posthog-rs: minor
---

feat(flags): support group-targeted and mixed-targeting feature flags in local evaluation

Adds local evaluation support for pure group flags (where `aggregation_group_type_index` is set at the flag level) and mixed-targeting flags (where individual conditions can override the aggregation per condition). `LocalEvaluator::evaluate_flag`, `evaluate_flag_simple`, and `evaluate_all_flags` now take `groups` and `group_properties` parameters; `match_feature_flag` and `match_feature_flag_with_context` have updated signatures. Backwards-incompatible at the public-API level — see PR description for migration notes.

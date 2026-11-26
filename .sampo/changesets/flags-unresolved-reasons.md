---
cargo/posthog: minor
---

`FeatureFlagEvaluations::unresolved_flags()` lists the flags that have a local definition but no value after `evaluate_flags`, each with an `UnresolvedFlagReason`: `ExperienceContinuity`, `UnsupportedDefinition`, `UnresolvedDependency` or `MissingContext`. This tells a flag that local evaluation couldn't decide apart from one that doesn't exist. A flag the `/flags` fallback resolves isn't listed, and neither is a flag outside `flag_keys`. Reading the map makes no request and sends no event. Reading an unresolved flag with `is_enabled` or `get_flag` now reports `local_evaluation_inconclusive` instead of `flag_missing` in `$feature_flag_called`. `InconclusiveMatchError` gains a `reason` field. When several causes apply, it holds the first of experience continuity, unsupported definition, unresolved dependency and missing context, as the evaluate-flags spec requires.

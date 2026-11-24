---
cargo/posthog: minor
---

Add `person_property_keys_known_absent` and `group_property_keys_known_absent` to `EvaluateFlagsOptions`. Local evaluation treats a listed key as absent, so `is_not_set` and the negated operators match and every other supported operator doesn't, without falling back to `/flags`. The condition value isn't parsed, compiled or validated, as on the flags service, so an invalid regex, number, version or date gives the same result as a valid one. Unsupported operators and a missing or null condition value stay inconclusive. Group keys only apply to their group type, cohort leaves and flag dependencies use the same listing, and person keys starting with `__posthog_person_metadata__` are ignored. When `/flags` is called, the request carries both lists. A list with more than 1000 keys, or a group type with more than 1000, makes `evaluate_flags` return `Error::InvalidArgument` without evaluating or sending anything.

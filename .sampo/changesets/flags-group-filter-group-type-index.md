---
cargo/posthog: patch
---

Local evaluation of definitions loaded by `LocalEvaluator` now reads a condition's `type: "group"` filter from the group type its own `group_type_index` names, as the flags service does, instead of the group the condition aggregates on. With a usable key for that group, the filter reads its supplied properties and known-absent keys, and a missing key stays inconclusive. Without one, the supplied properties are complete, so a missing key is absent. An index the group type mapping doesn't know makes the filter not match when the request has any group context, and reads no properties otherwise.

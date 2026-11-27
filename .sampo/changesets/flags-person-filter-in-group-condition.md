---
cargo/posthog: patch
---

Local evaluation now reads a `type: "person"` filter in a condition aggregated on a group, at the top of the condition or as a cohort leaf, from the person properties and the person known-absent keys, as the flags service does, instead of the group's properties. This covers direct evaluation and a group flag evaluated as a flag dependency. Group filters and group cohort leaves still read the condition's group.

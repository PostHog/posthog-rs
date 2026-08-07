---
cargo/posthog-rs: patch
---

Fix local flag evaluation for projects that use cohorts.

The `/flags/definitions/?send_cohorts` payload maps each cohort ID directly to its property group (`{"type": "AND"|"OR", "values": [...]}`), but `Cohort` required non-existent `id`/`name`/`properties` fields, so the whole `LocalEvaluationResponse` failed to deserialize and no flags loaded at all. `Cohort` now deserializes the real payload, and cohort matching recurses through nested AND/OR groups and cohort references instead of flattening them.

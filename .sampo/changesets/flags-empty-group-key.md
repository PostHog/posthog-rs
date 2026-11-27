---
cargo/posthog: patch
---

Local evaluation now treats an empty group key as no group key, matching the PostHog flags service. A condition aggregated on that group type is skipped, so a flag whose conditions are all skipped this way resolves `false` locally without a `/flags` request, whether it's evaluated directly or as a dependency, and the empty key is never used for bucketing. Before, an empty key made the flag inconclusive when its group properties were missing, and was used as a real key when they were supplied.

---
cargo/posthog: patch
---

Local evaluation now evaluates a flag dependency exactly as if the referenced flag were evaluated directly with the same inputs, per PostHog/sdk-specs#250. The dependency reads the caller's person properties, its own cohort and flag dependency filters, and its own aggregation and bucketing, so a person parent such as `plan exact "pro"` no longer makes the dependent flag inconclusive. This also changes local results without known-absent keys. A dependency cycle, a dependency chain more than 100 flags deep (counting the cohort levels it goes through), or more than 1000 dependency evaluations for one flag stays inconclusive, and a dependency referenced more than once is evaluated once.

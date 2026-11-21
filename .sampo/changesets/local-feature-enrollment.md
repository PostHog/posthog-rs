---
cargo/posthog: minor
---

Local flag evaluation supports early-access feature enrollment. For a flag with `feature_enrollment`, the person property `$feature_enrollment/<flag key>` decides the flag before its release conditions: `true` or `"true"` is enrolled, any other value is opted out, and without it the flag is inconclusive and falls back to the server. Adds `FeatureFlagFilters::feature_enrollment`; code that builds `FeatureFlagFilters` with a struct literal needs the new field or `..Default::default()`.

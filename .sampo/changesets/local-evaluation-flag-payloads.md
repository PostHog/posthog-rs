---
cargo/posthog-rs: patch
---

Surface feature flag payloads from local evaluation.

The definitions manifest carries each flag's payloads, but locally evaluated flags always reported `payload: None`, so `FeatureFlagEvaluations::get_flag_payload` returned nothing for them. When `flag_keys` was fully covered locally there was also no `/flags` round trip left to recover the payload from. Local evaluation now resolves the payload for the matched value (`"true"` for a boolean match, the variant key for a multivariate one) and decodes it exactly like a `/flags` payload, so a flag returns the same payload whichever path evaluated it. As a result, `$feature_flag_called` events for locally evaluated flags now carry `$feature_flag_payload`, matching remote evaluation.

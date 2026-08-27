---
cargo/posthog-rs: patch
---

Match local feature flag string properties with the same case folding and string coercion as the flags service. Substring and prefix/suffix operators now fold ASCII only, while `exact` and `is_not` use Unicode lowercase after stringifying both operands.

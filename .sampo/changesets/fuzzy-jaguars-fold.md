---
cargo/posthog-rs: patch
---

Match local feature flag properties with the same case folding, string coercion, and boolean-like filter precedence as the flags service. Substring and prefix/suffix operators now fold ASCII only, while `exact` and `is_not` use the service's truthiness handling before Unicode-lowercase comparison.

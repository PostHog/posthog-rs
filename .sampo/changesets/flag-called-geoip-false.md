---
cargo/posthog-rs: patch
---

A per-call `disable_geoip: Some(false)` in `EvaluateFlagsOptions` now sends `$geoip_disable: false` on `$feature_flag_called` events, so it beats a request context `$geoip_disable`. Before, it sent nothing and the context value applied. `Some(true)` and `None` are unchanged.

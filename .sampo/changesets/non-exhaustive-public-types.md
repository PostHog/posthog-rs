---
cargo/posthog-rs: major
---

Mark more public types `#[non_exhaustive]`: `FeatureFlagsResponse`, `FlagDetail`, `FlagMetadata`, `FlagReason`, `LocalEvaluationResponse`, `CaptureResponse` and `EventResult`. A `match` on `FeatureFlagsResponse` needs a wildcard arm, and the structs no longer accept struct literals. Build a `LocalEvaluationResponse` with the new `LocalEvaluationResponse::new`.

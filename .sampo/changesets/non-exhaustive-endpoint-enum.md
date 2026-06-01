---
cargo/posthog-rs: minor
---

Mark public `Endpoint` enum as `#[non_exhaustive]` and add `CaptureV1` variant. Downstream code that exhaustively matches on `Endpoint` without a wildcard arm will need to add `_ => { ... }`.

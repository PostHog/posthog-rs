---
cargo/posthog-rs: patch
---

Fall back to remote feature flag evaluation for unsupported or incompatible regex syntax instead of returning incorrect local results. Invalid local patterns no longer match `not_regex`.

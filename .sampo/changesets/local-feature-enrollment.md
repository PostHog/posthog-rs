---
cargo/posthog: minor
---

Local flag evaluation supports early-access feature enrollment. For a flag with `feature_enrollment`, the person property `$feature_enrollment/<flag key>` decides the flag before its release conditions: `true` or `"true"` is enrolled, any other value is opted out, and without it the flag is inconclusive and falls back to the server. Flags with experience continuity are also inconclusive locally, since only the server knows their persisted value.

---
cargo/posthog: patch
---

Local flag evaluation no longer stops at the first property filter in a condition it can't check. A later filter that doesn't match now rules the condition out, like the other SDKs, so a flag that can't match is `false` instead of inconclusive and doesn't need a call to `/flags`.

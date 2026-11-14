---
cargo/posthog: minor
---

Add per-capture fallback properties to the stateful client, with lower precedence than registered properties, platform context and SDK metadata. Fallback properties are assembled before before-send hooks and apply only to the supplied capture.

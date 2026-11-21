---
cargo/posthog: patch
---

Local flag evaluation treats a filter type it cannot evaluate, such as a cohort's `person_metadata` filter on first seen, as inconclusive so the flag falls back to the server, instead of matching it against a person property with the same key.

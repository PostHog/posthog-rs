---
cargo/posthog: patch
---

Local evaluation of definitions loaded by `LocalEvaluator` is now inconclusive for any active flag whose holdout excludes anyone, person or group flag, with or without a group key, so it falls back to `/flags` (or stays unresolved with `only_evaluate_locally`). This SDK doesn't evaluate holdouts, so it no longer resolves such a flag while the flags service would return the holdout variant. A flag that depends on it is inconclusive too. A 0%, missing or malformed holdout is ignored as before, and early-access enrollment still decides a flag first.

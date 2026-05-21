---
cargo/posthog-rs: patch
---

fix(flags): reject semver values with leading zeros in local evaluation

Per semver 2.0.0 §2, numeric identifiers must not include leading zeros. Values like `1.07.3` are not valid semver and no longer match targeting conditions. Both override values and flag values are validated; invalid inputs surface an `InconclusiveMatchError` so the condition does not match.

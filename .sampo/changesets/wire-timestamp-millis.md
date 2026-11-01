---
cargo/posthog-rs: patch
---

The batch `created_at` and the `PostHog-Request-Timestamp` header now use the same format as event timestamps: UTC with a `Z` and 3, 6 or 9 fractional-second digits. Before, they used a `+00:00` offset and left out the fraction when the clock was on a whole second.

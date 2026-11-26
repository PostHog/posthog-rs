---
cargo/posthog: patch
---

Local evaluation of an early-access flag now treats a person listed as known absent for `$feature_enrollment/<flag key>` as not enrolled, even when a value was supplied for that key. The holdout and release conditions decide the flag instead, and the missing enrollment no longer makes it inconclusive.

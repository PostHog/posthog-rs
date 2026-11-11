---
cargo/posthog-rs: patch
---

Relative date filters in local flag evaluation now follow the server: months and years are calendar steps instead of 30 and 365 days, `7d` without a leading `-` is accepted, and a number of 10,000 or more is inconclusive instead of panicking.

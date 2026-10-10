---
cargo/posthog-rs: major
---

PostHog sets `$lib` and `$lib_version` from the `PostHog-Sdk-Info` request header, `posthog-rs/<version>`, so stored events always report `posthog-rs`. The SDK no longer sends `$lib_version__major`, `$lib_version__minor` or `$lib_version__patch`; filter on `$lib_version` instead.

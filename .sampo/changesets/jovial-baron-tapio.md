---
cargo/posthog: minor
---

Load stateful remote configuration asynchronously, persist supported settings, gate startup feature flag requests with hasFeatureFlags, and report load failures through `on_error` as `PostHogError::RemoteConfig`.

---
cargo/posthog: major
---

The SDK sends `posthog-rs/<version>` in the `PostHog-Sdk-Info` and `User-Agent` headers on every capture request, and adds `$lib` and `$lib_version` to each event's properties before `before_send` runs. Both are sent as they are at send time, and PostHog stores them. PostHog uses the header only to fill in what an event leaves unset: both properties when `$lib` isn't set, and the version when `$lib` is `posthog-rs` and `$lib_version` isn't set.

A `$lib` counts as set only when it's a non-empty string. If `$lib` isn't set, the SDK sets `$lib` to `posthog-rs` and `$lib_version` to the crate version, replacing any `$lib_version` set on its own. If `$lib` is `posthog-rs` and `$lib_version` isn't set, the SDK adds the version. Any other `$lib` is sent with whatever `$lib_version` the event has, so set both when you name another library. Unlike the default 0.x path, the SDK doesn't add its `$lib_version` or version parts to another `$lib` you set, and it replaces a `null` or empty `$lib`. It adds `$lib_version__major`, `$lib_version__minor` and `$lib_version__patch` as numbers only when it adds its own `$lib_version`. MCP events send `$lib` as `posthog-rs-mcp` with the crate version as `$lib_version`, and no longer carry the version parts.

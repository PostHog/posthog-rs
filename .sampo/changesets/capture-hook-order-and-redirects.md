---
cargo/posthog-rs: minor
---

`before_send` now sees groups from `Event::add_group` in the `$groups` property and can change or remove them. For minimized `$feature_flag_called` events, the SDK trims properties to the allowlist before `before_send`, so properties the hook adds are sent. The capture and `/flags` HTTP clients the SDK builds follow only 307 and 308 redirects to the same scheme, host and port, at most 5 times; any other redirect fails the request. Shutdown and queue-full drop warnings now name the capture endpoint.

---
cargo/posthog-rs: minor
---

Allow supplying your own reqwest clients via the `http_client` and `blocking_http_client` client options. Applications that configure their own TLS backend, crypto provider, proxies, connection pool, or timeouts can now have the SDK reuse that client instead of the three it builds internally. A supplied client is used as-is, so `request_timeout_seconds` does not apply to it; leaving the options unset keeps the current behaviour.

Add a `background_transport` client option (default `true`). Setting it to `false` starts no background batching worker and no blocking HTTP client, for applications that only use `capture_immediate`/`capture_batch_immediate` and must not spawn a thread or block while dropping the client. Fire-and-forget `capture`/`capture_batch`/`alias`/`group_identify` then drop their events with a single warning, while immediate capture and feature flag evaluation work normally.

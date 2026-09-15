---
cargo/posthog-rs: minor
---

Allow supplying your own reqwest clients via the `http_client` and `blocking_http_client` client options. Applications that configure their own TLS backend, crypto provider, proxies, connection pool, or timeouts can now have the SDK reuse that client instead of the three it builds internally. A supplied client is used as-is, so `request_timeout_seconds` does not apply to it; leaving the options unset keeps the current behaviour.

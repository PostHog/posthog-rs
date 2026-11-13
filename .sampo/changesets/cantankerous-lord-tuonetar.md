---
cargo/posthog-rs: minor
---

`Client::get_remote_config_payload(flag_key)` reads a remote config flag's payload from the PostHog API, with the client's `secret_key`. It works with the async and the blocking client, does not need local evaluation, and sends no `$feature_flag_called` event. The payload comes back as `serde_json::Value`. PostHog stores it as a JSON string, so the SDK parses a string value a second time and returns the string unchanged when that fails. A client that is disabled returns `Ok(None)` without a request. A missing `secret_key` returns `Error::Unauthorized`. A failed request or a non-2xx status returns an `Error` and is not retried. A project secret API key (`phs_...`) gets an encrypted payload redacted, so use a personal API key (`phx_...`) to read it decrypted.

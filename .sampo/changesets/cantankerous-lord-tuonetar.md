---
cargo/posthog-rs: minor
---

`Client::get_remote_config_payload(flag_key)` reads the payload of a remote config feature flag, with no distinct ID. It works with the async and the blocking client, and needs the `secret_key` option. A failed request or a non-2xx status returns an `Error`, and a flag with no payload returns `Ok(None)`.

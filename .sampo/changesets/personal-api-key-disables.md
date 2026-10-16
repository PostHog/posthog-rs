---
cargo/posthog-rs: minor
---

A personal API key (`phx_...`) passed as the project `api_key` disables the client and logs one error, as an empty key does. The empty-key message is now logged at the error level, and a client built with `disabled(true)` logs nothing about its key.

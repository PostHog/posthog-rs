---
posthog-rs: minor
---

Panic autocapture. `install_panic_hook(client)` installs a process-wide `std::panic` hook that captures panics as personless `$exception` events through the client's transport, then calls the previously installed hook. Each event carries the panic payload, the panic-site location (`$exception_panic_file`/`_line`/`_column`), and a call-site stack trace honoring `capture_stacktrace`. It works on both the async and blocking clients with no async runtime present, because capture routes through the background worker thread. Gated by the default-on `error-tracking` feature.

New `ErrorTrackingOptions`: `panic_flush_timeout_ms` (default 2000) bounds how long the hook blocks the panicking thread waiting for the event to flush before letting the panic proceed — kept short and separate from `shutdown_timeout_ms` so a slow or unreachable PostHog can't freeze the crashing process or delay its panic message.

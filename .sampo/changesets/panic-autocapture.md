---
posthog-rs: minor
---

Panic autocapture, on by default. When the global client is initialized with `init_global`, a process-wide `std::panic` hook is installed automatically (gated by the default-on `error-tracking` feature). It captures panics as personless `$exception` events through the global client, then calls the previously installed hook. Each event carries the panic payload, the panic-site location (`$exception_panic_file`/`_line`/`_column`), and a call-site stack trace honoring `capture_stacktrace`. Capture routes through the background worker thread, so it needs no async runtime and a panicking `before_send` hook can't abort the process.

Set `ErrorTrackingOptions::capture_panics` to `false` to opt out of the automatic global install. To capture panics through a *standalone* (non-global) `Client`, call `install_panic_hook(Arc<Client>)` yourself — the hook is `'static`, so it takes an `Arc` to keep the client alive for the process.

New `ErrorTrackingOptions`: `capture_panics` (default `true`) and `panic_flush_timeout_ms` (default 2000) — the latter bounds how long the hook blocks the panicking thread waiting for the event to flush before letting the panic proceed, kept short and separate from `shutdown_timeout_ms` so a slow or unreachable PostHog can't freeze the crashing process or delay its panic message.

Delivery is best-effort: the just-captured `$exception` is flushed ahead of queued retries, but under sustained backpressure (a slow/unreachable endpoint or a large capture backlog) it may not be sent within the bound before the process exits.

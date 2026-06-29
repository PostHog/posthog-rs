---
posthog-rs: minor
---

Panic autocapture (opt-in). Set `ErrorTrackingOptions::capture_panics` to `true` and initialize the global client with `init_global`, and the SDK installs a process-wide `std::panic` hook that captures panics as personless `$exception` events through the global client, then calls the previously installed hook. Each event carries the panic payload, the panic-site location (`$exception_panic_file`/`_line`/`_column`), and a call-site stack trace honoring `capture_stacktrace`. Capture routes through the background worker thread, so it needs no async runtime and a panicking `before_send` hook can't abort the process. Gated by the default-on `error-tracking` feature.

Panic autocapture is **global-only**: a panic hook is process-global (`std::panic::set_hook`), so it pairs with the process-global client. There is intentionally no per-`Client` panic API for now.

New `ErrorTrackingOptions`: `capture_panics` (default `false`). The flush the hook performs on the panicking thread is bounded by a fixed, short timeout (2s) so a slow or unreachable PostHog can't freeze the crashing process or delay its panic message.

Delivery is best-effort: the just-captured `$exception` is flushed ahead of queued retries, but under sustained backpressure (a slow/unreachable endpoint or a large capture backlog) it may not be sent within the bound before the process exits.

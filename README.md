# PostHog Rust

[![Crates.io](https://img.shields.io/crates/v/posthog-rs.svg)](https://crates.io/crates/posthog-rs)
[![Documentation](https://docs.rs/posthog-rs/badge.svg)](https://docs.rs/posthog-rs)

The official Rust SDK for [PostHog](https://posthog.com). See the main [PostHog docs](https://posthog.com/docs) for more information.

Upgrading from a 0.x release? Read the [1.0 migration guide](docs/migration-0.x-to-1.0.md).

SDK usage examples and code snippets live in the official documentation so they stay up to date.

## Documentation

- [Rust library docs](https://posthog.com/docs/libraries/rust)

## Request context

With the optional `tower` feature, `tower::PostHogLayer` reads
`X-PostHog-Distinct-Id` and `X-PostHog-Session-Id` for Tower and Axum services.
Header values are trimmed, stripped of control characters, and limited to 1000
characters. Missing or invalid values are omitted. No session ID is generated.

```rust
use axum::{routing::get, Router};
use posthog_rs::{tower::PostHogLayer, RequestContext};

let posthog = std::sync::Arc::new(posthog_rs::client("phc_project_key").await);
let app: Router = Router::new()
    .route("/work", get(move || {
        let posthog = posthog.clone();
        async move {
            let context = RequestContext::current().unwrap_or_default();
            posthog.capture(context.event("work_completed"));
            "ok"
        }
    }))
    .layer(PostHogLayer::new());
```

In an application handler, pass `context.event("work_completed")` to
`client.capture(...)`. It uses the context identity or the existing personless
capture behavior when no identity exists. Existing events keep their explicit
identity and properties. The context adds defaults, including `$session_id`,
`$request_method`, and `$request_path`. Query strings are not captured.

Tracing headers are client-controlled analytics context. Never use them for
authentication, authorization, or account lookup. Place an application-owned
`RequestContext` in the request extensions before the PostHog layer runs to give
that identity priority. After authentication in a handler, clone the current
context, set `.distinct_id(authenticated_id)`, and run the remaining work with
`.scope(async { ... }).await`.

`RequestContext::current()` returns a snapshot inside a Tokio task scope.
Captures, batches, immediate captures, and exceptions take their defaults
before background delivery. `client.evaluate_flags_with_context(&context,
options)` uses the context identity. An absent identity returns an empty flag
result without a network request. Flag access events retain the context from
evaluation, including when the result is read after the request ends.

Spawned tasks and streaming response bodies do not inherit context. Pass a
context clone and use `.scope(...)` explicitly for work that must retain it.
Request completion, errors, panics, and cancellation remove the scoped context.
The middleware preserves application responses and errors and does not capture
exceptions automatically.

Without Tower or an async client, construct `RequestContext` directly and use
`context.event(...)`, `context.apply_to_event(&mut event)`,
`context.exception_options(options)` with `client.capture_exception_with(...)`,
or `client.evaluate_flags_with_context(&context, options)`. The exception helper
requires `error-tracking`. Explicit event and exception values keep priority.
Blocking applications use these explicit helpers; they have no ambient scope.

This integration adds analytics context to events. It does not add attributes
to application-owned tracing or OpenTelemetry spans or install a trace provider.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for local setup and test instructions.

## Releasing

See [RELEASING.md](RELEASING.md).

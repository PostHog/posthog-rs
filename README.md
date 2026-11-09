# PostHog Rust

[![Crates.io](https://img.shields.io/crates/v/posthog.svg)](https://crates.io/crates/posthog)
[![Documentation](https://docs.rs/posthog/badge.svg)](https://docs.rs/posthog)

The official Rust SDK for [PostHog](https://posthog.com), published as [`posthog`](https://crates.io/crates/posthog) starting with 1.0. The [`posthog-rs`](https://crates.io/crates/posthog-rs) package remains available as a compatibility crate that re-exports `posthog`. See the main [PostHog docs](https://posthog.com/docs) for more information.

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
use posthog::{tower::PostHogLayer, RequestContext};

let posthog = std::sync::Arc::new(posthog::client("phc_project_key").await);
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
`RequestContext::option` adds a capture option to every event in the context,
unless the event sets that option itself.

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

MCP analytics (the `mcp` feature) applies the context to every MCP event over
HTTP, although `rmcp` runs each request handler in a task of its own. It reads
the `RequestContext` that `PostHogLayer` or your middleware placed in the
request extensions. A context scoped around the code that sends the event takes
priority. The MCP identity from the `identify` callback stays the distinct ID
and wins over the context identity. An anonymous request keeps its MCP session
ID as the distinct ID and the `process_person_profile: false` option. Context
properties and options fill only keys the event leaves unset, before the MCP
`before_send` hook runs. Options from `McpOptionsBuilder::event_options` and
`ToolCallRequest::event_option` win over context options, and the call's own
option wins over the callback's.

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

## Application HTTP context

With the `async-client` feature, use `PostHogReqwestClient` to pass the current request's
analytics IDs to selected application hosts. This is separate from event ingestion.

```rust
use posthog::{PostHogReqwestClient, RequestContext};

async fn example() -> Result<(), reqwest::Error> {
    let http = PostHogReqwestClient::new(
        reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)),
        ["api.example.com"],
    )?;
    let request = http.client().post("https://api.example.com/work")
        .body("application payload").build()?;
    let response = RequestContext::new().distinct_id("person").session_id("session")
        .scope(http.execute(request)).await?;
    Ok(())
}
```

Build requests with `http.client()`, then send them with `http.execute(request)`.
Calling reqwest's `send` directly does not add context. The helper reads context when
the execute future runs, so each request uses the active IDs. A spawned task needs
an explicit `RequestContext::scope` with the context it must carry.

Host entries match exactly after trimming and case normalization. Use hostnames only,
without schemes, ports, paths, or wildcards. The helper replaces stale PostHog headers
on matched requests, omits missing or invalid IDs, and leaves other requests unchanged.
It does not create IDs. Do not set PostHog tracing headers as reqwest client default
headers: those application-owned defaults can apply to any host.

This opt-in client **disables automatic redirects**, including same-host redirects.
It returns the original redirect response. This prevents analytics IDs from following
a redirect to another host. If the application follows a redirect, build a new request
for the destination and pass it to `execute` to check the host again. Use a separate
client for automatic redirects. Other builder settings and request fields are retained.
The helper returns normal HTTP responses and reqwest errors to the caller.

This integration supports asynchronous reqwest 0.13 requests. It does not install a
global client, change an existing application client, or add OpenTelemetry span support.

# PostHog Rust

[![Crates.io](https://img.shields.io/crates/v/posthog.svg)](https://crates.io/crates/posthog)
[![Documentation](https://docs.rs/posthog/badge.svg)](https://docs.rs/posthog)

The official Rust SDK for [PostHog](https://posthog.com), published as [`posthog`](https://crates.io/crates/posthog) starting with 1.0. The [`posthog-rs`](https://crates.io/crates/posthog-rs) package remains available as a compatibility crate that re-exports `posthog`. See the main [PostHog docs](https://posthog.com/docs) for more information.

Upgrading from a 0.x release? Read the [1.0 migration guide](docs/migration-0.x-to-1.0.md).

SDK usage examples and code snippets live in the official documentation so they stay up to date.

## Documentation

- [Rust library docs](https://posthog.com/docs/libraries/rust)
- Stateful application client (`stateful` feature): local anonymous identity, capture/screen, native hooks and persistent delivery.

## Stateful remote configuration

The `stateful` client loads remote configuration asynchronously at initialization,
without delaying readiness. Settings use the configured persistence scope and
survive identity changes and reset. Failed refreshes retain the last valid settings;
`Persistence::Memory` keeps them in memory only.

Startup feature flags load after remote configuration. A successful
`hasFeatureFlags: false` response skips preload and clears stale server flags while
preserving bootstrap values. Missing or unavailable settings fall back to loading
flags. `Config::preload_feature_flags = false` suppresses startup flag loading, not
remote configuration. Explicit and targeting-triggered flag refreshes remain available.
Project-request failures are reported through `Config::on_error`. Project cache
writes are best effort: when settings exceed state-storage bounds, live settings
remain available and `on_error` receives a storage diagnostic. Successful state
drains describe mandatory state, not guaranteed cache retention.

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
unless the event sets that option itself. A context `process_person_profile`
does not set `process_person_profile` on identify-style events (`$identify`,
`$create_alias`, `$merge_dangerously`, `$groupidentify`, `$set` and
`$set_once`, including the events `alias` and `group_identify` send) whose
distinct ID the SDK did not generate. Those default to `true` unless the event
sets the option.

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

## Transactional email

`client.emails()` sends one email per call from your backend, from an email
template made transactional in PostHog or from content your code rendered. It
needs `secret_key` set to a project secret API key (`phs_...`) with the
`transactional_email:write` scope, which the "Send transactional email" preset
in your project settings gives. Keys made with `hog_flow:write` still work.
Requests go to your region's app host, derived from a
PostHog Cloud `host`. Set `app_host` when `host` is a reverse proxy: the proxy
must not receive the secret key.

```rust
use posthog::{ClientOptionsBuilder, CodeEmail, TemplateEmail};
use serde_json::json;

let options = ClientOptionsBuilder::default()
    .api_key("phc_project_key".to_string())
    .secret_key("phs_project_secret_key")
    .host("https://eu.i.posthog.com")
    .build()?;
let posthog = posthog::client(options).await;

// A template made in PostHog, by its ID.
let sent = posthog
    .emails()
    .send(
        TemplateEmail::new("jane@example.com", "0190f6d2-7c1e-7b3a-9f5e-1c2d3e4f5a6b")
            .template_data(json!({ "name": "Jane" }))
            .distinct_id("user_123")
            .idempotency_key("welcome:user_123"),
    )
    .await?;

// Content your code rendered, named by a key. For a React email, render it
// to HTML in your app first.
posthog
    .emails()
    .send(
        CodeEmail::new("jane@example.com", "password-reset", "Reset your password")
            .html(rendered_html)
            .distinct_id("user_123"),
    )
    .await?;

// What happened to the first email.
let status = posthog.emails().get(&sent.id).await?;
```

A result means PostHog queued the email, not that it was delivered. Pass
`distinct_id` so the email's events go to your user. Every attempt of a send
carries the same idempotency key, so the client's retries never send twice;
pass an `idempotency_key` that names the email when your own job runner
retries too, and set `max_retries(0)` on the send. A failed send returns an
`EmailError`: branch on `code()`, such as `quota_limited`, or on `kind()`.

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

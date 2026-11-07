# Stateful client

Enable the optional `stateful` feature for applications that keep a local anonymous
identity and a persistent delivery queue. Existing root-level server APIs and defaults
are unchanged.

```rust
use posthog::stateful::{Capture, Client, Config};
use serde_json::json;
use std::collections::HashMap;

let config = Config::new(std::env::var("POSTHOG_PROJECT_TOKEN").expect("Set POSTHOG_PROJECT_TOKEN"));
let client = Client::init_blocking(config);

let mut event = Capture::new("button clicked");
event.properties.insert("button".into(), json!("save"));
let admission = client.capture(event);
let screen_admission = client.screen("Settings", HashMap::new());

let drain = client.flush_blocking();
client.shutdown_blocking(None);
```

`capture("event name")` is shorthand for an event without caller properties.
`Capture` also accepts wire options, typed groups, a UTC timestamp and an event UUID.
Its identity comes from the client. `get_anonymous_id()` and `get_distinct_id()` read
memory synchronously. New anonymous IDs use UUIDv7; an anonymous bootstrap seeds a
fresh storage scope but cannot replace restored identity.

## Initialization and storage

Both `init_blocking(config)` and, with `async-client`, `Client::init(config).await`
return `Client` directly after local initialization. Async initialization uses a
standard initialization thread and does not require or create a Tokio runtime.
Cancelling initialization leaves cleanup of an unclaimed client on that thread,
even if readiness was already signalled.

Invalid configuration or local delivery setup failure reports through `on_error`
and disables that client. Disabled capture/screen/drain calls are safe no-ops;
identity getters return empty strings. `is_disabled()` is available for diagnostics.
A plausible but incorrect project token is a delivery failure, not something local
initialization can authenticate.

Persistence defaults to the OS local application-data directory, scoped by host,
project token and instance name. Set `Config::persistence` to `Persistence::Directory`
for an application-selected root or `Persistence::Memory` to disable disk storage.
State and queued events share the namespace lease. Storage errors report through
`on_error` and retain enabled, bounded memory-backed operation; unreadable or
unsupported state is preserved rather than overwritten. State and event persistence
can degrade independently.

Initialization does not wait for network connectivity or backlog delivery. Recovery
may temporarily reserve queue capacity after readiness. A ready client can therefore
report `CaptureOutcome::Full`; readiness is not a promise of free queue capacity.

## Capture and hooks

Capture and screen are synchronous. The SDK snapshots identity, applies cached
platform context and SDK defaults, runs hooks, converts the event, then attempts
bounded admission. It performs no filesystem or network work on this path. OS
context is discovered during initialization; embedding applications can provide
`platform_context`, `sdk_name` and `sdk_version` in configuration.

Caller properties win over context/default properties. `$set`, `$set_once`, `$groups`
and `$group_set` objects fill missing inner keys using the existing Rust rules.
Typed groups win per group type. Explicit options take precedence over legacy
property options. The SDK also supplies `process_person_profile: false` for anonymous
captures; this option takes precedence even over a caller-supplied legacy
`$process_person_profile` property. To override it for an event, set `Capture.options`
explicitly:

```rust
let mut event = Capture::new("button clicked");
event.options.insert("process_person_profile".into(), json!(true));
client.capture(event);
```

Session/window properties are lifted into wire fields using the shared converter.
Ambient server `RequestContext` is not applied to the stateful client.

Register native hooks with `Config::before_send`. Hooks see the assembled event and
run in registration order on the capture caller, outside client-state locks. They
may modify the event or return `None` to drop it. A panic stops the chain and drops
the event. Each mutable hook is serialized across concurrent captures: do not wait
for another capture of this client from inside a hook. Recursive same-thread capture
returns `Reentrant` rather than bypassing privacy hooks. Closing from a hook returns
a deferred drain and prevents the outer capture from being admitted.

After hooks, custom null-valued object members are omitted recursively, including
inside arrays. Null array slots, empty containers and other scalar values are kept.
Reserved person/group mutation payloads (`$set`, `$set_once`, `$group_set`, `$groups`),
wire options and envelope fields retain their separate semantics. Cleanup does not
restore properties removed by a hook. Capture-time `$lib` and `$lib_version` survive
persistence, recovery and retries; SDK headers continue to describe the sending Rust
SDK. Live ingestion of that attribution requires a backend that honors per-event
SDK properties rather than overwriting them from headers.

`screen(name, properties)` uses the same pipeline for `$screen`. The explicit name
wins over a conflicting `$screen_name` before hooks; hooks retain their final say.
Screen capture does not install platform navigation observers.

## Admission and drains

`CaptureOutcome::Accepted { uuid }` reports local admission, not persistence or server
ingestion. A full count/serialized-byte budget rejects new work without evicting
already admitted events. Invalid inputs, hook drops, reentrancy, closure and disabled
configuration have distinct outcomes. The byte limit bounds serialized queued data,
not all caller allocations or HTTP/compression buffers.

`flush_blocking()` and `flush().await` return separate state-persistence and delivery
outcomes. They capture a state revision and attempt earlier delivery work; retries
and persisted backlog can remain. Cancelling an async wait does not cancel requested
work. The async methods require `async-client`, but no Tokio runtime.

`shutdown_blocking(timeout)` and `shutdown(timeout).await` close admission and request
both workers to close before waiting. Pass `None` for `Config::shutdown_timeout`
(default 30 seconds), or `Some(Duration::from_secs(5))` for a per-call override.
Explicit durations are clamped to one day; zero allows no further scheduled shutdown
drain. The first call selects a shared deadline measured from that call; repeated
calls cannot restart or extend the budget. Cancellation leaves the client closing,
and another shutdown call can wait for completion. Worker callbacks and capture
hooks request progress/closure but return `Deferred` instead of waiting.

The budget is best-effort, **not a hard return deadline**. Work already underway,
filesystem I/O, callbacks and final thread joins can exceed it. Async shutdown
awaits worker progress but still performs synchronous final joins, which can block
polling during thread cleanup. External destruction also waits for workers using
the configured budget unless shutdown already began. Destruction inside a worker
callback requests closure without joining; the workers retain their own resources
until they finish. Durable retryable events can remain on disk for the next instance.

See [the runnable example](../examples/stateful.rs) and
[delivery/storage details](stateful-delivery.md).

---
title: "Migrate the Rust SDK from 0.x.x to 1.0.0"
sidebar: Docs
showTitle: true
---

The Rust SDK 1.x.x sends every event to the [capture v1](https://posthog.com/docs/api/capture-v1) endpoints, `/i/v1/analytics/events` and `/i/v1/ai/events`.

Use the top right copy page button to work through this with your agents.

You don't need to make every change on the day you upgrade your SDK. Required changes fall in these categories:

- **Before you upgrade**: you should make these changes before you upgrade your SDK to 1.x.x.
- **During your upgrade**: These are changes that you need to make in the same PR that upgrades your SDK version.
- **Follow ups**: These are changes that can come in follow up PRs because 1.x.x has limited backward compatibility for methods that are to be deprecated.

## Checklist

Read the checklist first, then the sections that apply to you.

**Before you upgrade**

- [ ] You send events through your own proxy → [Prepare your proxy](#prepare-your-proxy)
- [ ] A proxy redirects capture or `/flags` requests → [Prepare your proxy](#prepare-your-proxy)
- [ ] You pass a personal API key (`phx_...`) as the project `api_key` → [Use a project token](#use-a-project-token)
- [ ] Your build environment has no C compiler → [Check your C compiler](#check-your-c-compiler)
- [ ] You run a self-hosted PostHog → [Prepare your proxy](#prepare-your-proxy)

**During your upgrade**

- [ ] You enable the `capture-v1` feature → [Update Cargo features](#update-cargo-features)
- [ ] You set `default-features = false` → [Update Cargo features](#update-cargo-features)
- [ ] You call `Uuid::new_v4` → [Update Cargo features](#update-cargo-features)
- [ ] You use `Endpoint::Batch`, `V1ErrorResponse`, or `personal_api_key` → [Fix removed APIs](#fix-removed-apis)
- [ ] You `match` on `Endpoint` or `FeatureFlagsResponse`, or build an SDK response type with a struct literal → [Fix removed APIs](#fix-removed-apis)
- [ ] You call `is_feature_enabled`, `get_feature_flag`, `get_feature_flag_payload`, or `get_feature_flags` → [Replace flag methods](#replace-flag-methods)
- [ ] You build `EvaluateFlagsOptions` with a struct literal → [Replace flag methods](#replace-flag-methods)
- [ ] You filter logs on the `posthog_rs` target, or match the SDK's log lines → [Update log filters](#update-log-filters)
- [ ] Your `before_send` hook returns a new `Event` → [Copy options in before_send](#copy-options-in-before_send)
- [ ] You send large `$ai_` events, or use the `$ai_` prefix for your own events → [Route AI events](#route-ai-events)
- [ ] You capture a cloned or reused `Event` more than once, or set a `uuid` with `Event::set_uuid` → [Check event IDs](#check-event-ids)
- [ ] You set timestamps in the future, or read the timestamps the SDK sends → [Check timestamps](#check-timestamps)
- [ ] You call `group_identify`, or pass an empty distinct ID to `Event::new` → [Fix identity calls](#fix-identity-calls)
- [ ] You send `$identify`, `$create_alias`, `$merge_dangerously`, `$groupidentify`, `$set`, or `$set_once` events, including through `alias` or `group_identify` → [Check identify-style events](#check-identify-style-events)
- [ ] You use `Event::new_anon`, `Event::add_group`, or `CaptureExceptionOptions::group` → [Check person processing](#check-person-processing)
- [ ] You set an option to a value of another type, such as `"false"` for a boolean → [Check option values](#check-option-values)
- [ ] You set `$set`, `$set_once`, `$groups`, or `$group_set` on an event and in a request context → [Check property precedence](#check-property-precedence)
- [ ] You set `$os`, `$is_server`, `$geoip_disable`, or a `$feature/<key>` property yourself, or change them in `before_send` → [Check property precedence](#check-property-precedence)
- [ ] You use `RequestContext`, with the async client, the blocking client, or the `mcp` feature → [Check request context](#check-request-context)
- [ ] You set `$lib` or `$lib_version` yourself, or use the `mcp` feature → [Check SDK identity](#check-sdk-identity)
- [ ] You send event names over 200 bytes, empty event names, or a `$session_id` or `$window_id` that isn't a string → [Check dropped events](#check-dropped-events)
- [ ] You send batches of large events → [Check batch sizes](#check-batch-sizes)
- [ ] You set `CaptureCompression` to `Deflate`, `Br`, or `Zstd` → [Check compression](#check-compression)
- [ ] You set `$trace_id` or `$span_id` on exceptions, or read `$debug_images` → [Check exceptions](#check-exceptions)
- [ ] You advance durable state after `capture_immediate`, or use an `on_error` hook → [Update error handling](#update-error-handling)
- [ ] You set `max_capture_attempts` or `retry_initial_backoff_ms`, or rely on their defaults → [Check retries](#check-retries)

**Follow ups**

- [ ] You depend on `posthog-rs` and import `posthog_rs` → [Rename the crate](#rename-the-crate)
- [ ] You set legacy option properties like `$process_person_profile` → [Move to event options](#move-to-event-options)
- [ ] You use the `mcp` feature → [Add MCP event options](#add-mcp-event-options)
- [ ] You capture `$ai_` events with `capture` → [Send AI events with capture_ai](#send-ai-events-with-capture_ai)

## Before you upgrade

### Prepare your proxy

1.x.x sends events to these endpoints:

| Events | Endpoint |
| --- | --- |
| Every event except those from the `capture_ai` methods | `/i/v1/analytics/events` |
| Events from the `capture_ai` methods | `/i/v1/ai/events` |

The analytics endpoint includes error tracking, `$feature_flag_called` events, historical migration, `before_send`, and terminal failures reported through `on_error`.

The `capture_ai` methods are `capture_ai`, `capture_ai_batch`, `capture_ai_immediate`, `capture_ai_batch_immediate`, and the global `posthog::capture_ai`.

If you send events through your own reverse proxy, update it before you upgrade:

- **Forward both routes.** Forward `/i/v1/analytics/events` and `/i/v1/ai/events` to PostHog.
- **Pass the headers through unchanged.** Pass `Authorization`, every `PostHog-*` header, `Content-Type`, `Content-Encoding`, and `User-Agent`. `Authorization` holds the project token, sent as `Bearer <token>`. PostHog answers `401` without `Authorization` and `400` without the others, and the batch fails.
- **Keep PostHog credentials.** If the proxy strips other credentials, keep `Authorization` values that start with `Bearer phc_`, `Bearer phs_`, or `Bearer phx_`. Also keep the exact `Bearer <token>` of an older project token without a prefix.
- **Accept request bodies up to 20 MB.** AI requests are often larger than 1 MB, the default limit in nginx and ingress-nginx. A `413` from the proxy fails the batch.
- **Pass the capture response through.** Return PostHog's status code and response body unchanged. A proxy that answers the capture path with its own `200` fails the batch.

PostHog's managed reverse proxy already does this. See the [proxy guides](https://posthog.com/docs/advanced/proxy).

A self-hosted PostHog must serve both `/i/v1/analytics/events` and `/i/v1/ai/events`. On self-hosted PostHog, a separate capture service serves the AI route.

The capture and `/flags` HTTP clients the SDK builds follow a redirect only when it's a `307` or `308` to the same scheme, host, and port. They follow at most 5 redirects in a row.

Any other redirect fails the request, so the SDK never sends the request body to another host. 0.x.x followed up to 10 redirects of any kind to any host, and local evaluation polling still does. If a proxy redirects capture requests to another host, set `host` to the final host.

A client you pass with `http_client` or `blocking_http_client` keeps its own redirect policy.

### Use a project token

A personal API key (`phx_...`) passed as the project `api_key` disables the client and logs one error, as an empty key does. PostHog never accepts a personal key for capture, so every request with it failed with a `401`.

An empty key now logs at the error level instead of warning. A client built with `disabled(true)` logs nothing about its key.

### Check your C compiler

Brotli and Zstandard support is always compiled in. `zstd-sys` builds C code with the `cc` crate, so the build needs a C compiler.

Every 0.x.x build already needed one for `aws-lc-sys`, the TLS crypto library.

## During your upgrade

### Update Cargo features

Remove `capture-v1` from your dependency features. Capture no longer needs a feature flag.

```toml
# 0.x.x while opting into V1 capture
posthog-rs = { version = "0.27", features = ["capture-v1"] }

# 1.x.x
posthog = "1"
```

The default features are the async client, error tracking, and TLS. For the blocking client, use `default-features = false`, and enable a TLS feature yourself when you send to an HTTPS endpoint.

```toml
# Async client with error tracking and TLS
posthog = "1"

# Blocking client with TLS
posthog = { version = "1", default-features = false, features = ["tls"] }
```

0.x.x enabled TLS even with `default-features = false`. In 1.x.x, choose one of these features:

- **`tls`.** It uses reqwest's default Rustls provider.
- **`tls-no-provider`.** Choose it when the application installs a process-level Rustls `CryptoProvider`. It avoids pulling in `aws-lc-rs` from this SDK, but constructing a client before installing a provider panics.

Cargo features are additive, so `tls-no-provider` only avoids the built-in provider when no dependency enables `tls`.

1.x.x no longer enables the `v4` feature of the `uuid` crate. If your application calls `Uuid::new_v4`, enable `v4` on its own `uuid` dependency.

The `posthog-rs` package takes the same features in 1.x.x (see [Rename the crate](#rename-the-crate)).

### Fix removed APIs

1.x.x removes these APIs:

| Removed | Use instead |
| --- | --- |
| The V0 capture implementation, with its `/i/v0/e/` and batch plumbing | Nothing |
| `Endpoint::Batch` | Nothing |
| `V1ErrorResponse` | `CaptureErrorResponse` |
| `is_feature_enabled`, `get_feature_flag`, `get_feature_flag_payload`, `get_feature_flags` | `evaluate_flags` |
| The builder's `personal_api_key` alias | `secret_key` |
| `LocalEvaluationConfig::personal_api_key` | `LocalEvaluationConfig::secret_key` |

`Endpoint::Capture` now resolves to `/i/v1/analytics/events`.

For `evaluate_flags`, see [Replace flag methods](#replace-flag-methods). The single-flag methods and the builder's `personal_api_key` alias were already deprecated in 0.x.x.

Rename `V1ErrorResponse` to `CaptureErrorResponse`:

```rust
// 0.x.x with capture-v1
let response: Option<&posthog_rs::V1ErrorResponse> = failure.error_response();

// 1.x.x
let response: Option<&posthog::CaptureErrorResponse> = failure.error_response();
```

These types are now non-exhaustive:

| Type | What to change |
| --- | --- |
| `Endpoint` | Add a wildcard arm |
| `CaptureResponse`, `EventResult` | Remove struct literals |
| `EvaluateFlagsOptions` | Construct with `Default` |
| `FeatureFlagsResponse` | Add a wildcard arm |
| `FlagDetail`, `FlagMetadata`, `FlagReason`, `LocalEvaluationResponse` | Remove struct literals |

`Endpoint` has a new `CaptureAi` variant, so a `match` on it needs a wildcard arm. A `match` on the `FeatureFlagsResponse` enum needs a wildcard arm.

You can't build `CaptureResponse` or `EventResult` with a struct literal. Construct `EvaluateFlagsOptions` with `Default` (see [Replace flag methods](#replace-flag-methods)).

The `FlagDetail`, `FlagMetadata`, `FlagReason`, and `LocalEvaluationResponse` structs no longer accept struct literals. Build a `LocalEvaluationResponse` with `LocalEvaluationResponse::new` and assign its public fields.

Use `secret_key` terminology throughout configuration:

```rust
let options = posthog::ClientOptionsBuilder::default()
    .api_key("phc_project_token".to_string())
    .secret_key("phs_project_secret")
    .enable_local_evaluation(true)
    .build()?;
```

`secret_key` accepts either a project secret key (`phs_...`) or a personal API key (`phx_...`). Don't send this key as an event property.

### Replace flag methods

Call `evaluate_flags` once and read from the returned snapshot:

```rust
use posthog::EvaluateFlagsOptions;

let flags = client
    .evaluate_flags("user-123", EvaluateFlagsOptions::default())
    .await?;

if flags.is_enabled("new-checkout") {
    // Use the enabled feature.
}

let variant = flags.get_flag("checkout-variant");
let payload = flags.get_flag_payload("checkout-variant");
```

The blocking client uses the same calls without `.await`.

`EvaluateFlagsOptions` is now non-exhaustive. Construct it with `Default` and assign the fields you need instead of using a struct literal:

```rust
let mut options = EvaluateFlagsOptions::default();
options.disable_geoip = Some(true);
options.flag_keys = Some(vec!["new-checkout".to_string()]);
```

### Update log filters

The implementation's tracing target is now `posthog`, including when you use the `posthog-rs` compatibility crate. Update filters such as `RUST_LOG=posthog_rs=debug` to `RUST_LOG=posthog=debug`.

Update any log filter that matches the 0.x.x text of these lines:

- **A request that fails for good.** Without an `on_error` hook, it logs `posthog-rs: dropping <n> event(s) for <endpoint>: <error>`. 0.x.x logged the same line without `for <endpoint>`.
- **Queue-full and shutdown warnings.** They now name the endpoint too, for example `posthog-rs: event queue full for <endpoint> (capacity <n>); dropping events`.
- **A `before_send` hook that panics.** The SDK logs an error that names the event: `panic in PostHog before_send hook for event "<name>"; dropping event`. 0.x.x logged that line without `for event "<name>"`, and a panic in the `McpOptions` hook logged a different warning.

The error text can include PostHog's response body. Register an `on_error` hook to choose what to log.

### Copy options in before_send

Review your `before_send` hooks for the new event shape. Capture v1 reads per-event processing controls, such as person profile processing, from an `options` map that is sent next to `properties`.

`before_send` hooks can read and change options with `Event::options`, `insert_option`, and `remove_option`. To change an option, use `insert_option`.

A hook that returns a new `Event` must copy the options of the event it received, or the event is sent without them.

That includes the `process_person_profile` option that `Event::new_anon` sets to `false`. Without it, PostHog would create a person for the generated ID. It also includes the `true` the SDK sets on [identify-style events](#check-identify-style-events).

### Route AI events

1.x.x routes AI events to their own endpoint:

- **The endpoint.** `capture_ai`, `capture_ai_batch`, `capture_ai_immediate`, `capture_ai_batch_immediate`, and the global `posthog::capture_ai` mirror their analytics counterparts. They post to `/i/v1/ai/events` on their own background lane, batched by size.
- **Routing.** `capture` never reroutes by event name. Large AI events always need `capture_ai`.
- **Event names.** PostHog drops an event sent with `capture_ai` whose name doesn't start with `$ai_`. It reports it as a per-event `drop` with the detail `misrouted_event` inside a `200`.
- **Where the drop is reported.** That verdict reaches an `on_error` hook, where `CaptureFailure::endpoint()` says which lane, or the returned `CaptureSummary` for the immediate variants.
- **The `$ai_` prefix.** PostHog owns the `$ai_` prefix, so don't use it for your own events.
- **Flush and shutdown.** `flush` and `shutdown` drain both lanes.
- **Events over 8 MiB.** The SDK drops an event over the backend's 8 MiB per-event limit locally, on the background lane and in the immediate variants. On the background lane, that drop logs a warning with the event name and size, and it doesn't reach `on_error`. The immediate variants return that drop in the `CaptureSummary` with the detail `ai_event_too_big`.
- **Events over the request limit.** An event too large for the AI endpoint's request size limit gets HTTP `413` instead. `capture_ai_batch_immediate` returns `Error::BadRequest` and doesn't send the rest of the batch.

The AI lane has its own options:

| Option | Default |
| --- | --- |
| `capture_ai_compression` | Unset |
| `capture_ai_max_queue_size` | 1000 |

An unset `capture_ai_compression` sends AI bodies uncompressed. Set `CaptureCompression::Zstd`, the best fit for large JSON.

### Check event IDs

- **Generated IDs.** The SDK gives every event a UUIDv7 `uuid` and every request a UUIDv7 request ID.
- **When the SDK assigns a uuid.** The SDK assigns an event's uuid when you hand the event to `capture` or `capture_immediate`, not in `Event::new`. Capturing a cloned or reused `Event` twice now sends two uuids. In 0.x.x both copies shared one uuid, so PostHog kept only one of them.
- **Your own uuids.** A uuid set with `Event::set_uuid` is still sent as given on every capture. If you set your own, keep it unique.
- **Duplicates.** When two events in one request share a `uuid`, PostHog keeps the first and drops the later copies. The SDK reports each dropped copy through `on_error` and `CaptureSummary` with the reason `duplicate_event_uuid`. An entry's `EventResult::duplicates_dropped` counts the copies dropped with it. Older PostHog versions reject the whole request with HTTP `400` `duplicate_event_uuid` instead.
- **Duplicates of a retried event.** If the kept copy comes back `retry` in the background worker, those dropped copies aren't reported. PostHog still raises a `duplicate_event_uuid` ingestion warning in your project.

### Check timestamps

- **The format.** Every timestamp the SDK sends is UTC with a `Z` and 3, 6, or 9 fractional-second digits. That covers the event `timestamp`, the batch `created_at`, and the `PostHog-Request-Timestamp` header. A whole second is sent as `.000Z`, and finer digits from your timestamp or the clock are kept.
- **Future timestamps.** `Event::set_timestamp` accepts timestamps in the future. In 0.x.x it returned `Error::InvalidTimestamp` for a timestamp more than 1 s ahead. PostHog clamps far-future timestamps, as it does for every other SDK. The method keeps its `Result` return type.

### Fix identity calls

- **Empty distinct IDs.** `Event::new` with an empty or whitespace-only distinct ID creates an anonymous event, as `Event::new_anon` does. In 0.x.x it sent the empty ID, and PostHog dropped the event.
- **Long distinct IDs.** PostHog drops an event whose distinct ID is over 200 UTF-8 bytes, with the per-event result `distinct_id_too_large`. It reaches `on_error`, or the `CaptureSummary` of an immediate method. 0.x.x sent events to legacy capture, which shortened the ID to 200 characters and kept the event.
- **Group fallback.** `group_identify` sends the request context's distinct ID when the context has one, and `$<group_type>_<group_key>` otherwise. In 0.x.x it always sent `$<group_type>_<group_key>`.
- **Group sessions.** `group_identify` never adds the context session ID. In 0.x.x it did.
- **Long group IDs.** If `$<group_type>_<group_key>` is over 200 bytes, PostHog drops the event. To set a group with a longer key, capture a `$groupidentify` event yourself with `Event::new("$groupidentify", <your distinct ID>)` and the `$group_type`, `$group_key`, and `$group_set` properties.

### Check identify-style events

`$identify`, `$create_alias`, `$merge_dangerously`, `$groupidentify`, `$set`, and `$set_once` events default to `process_person_profile: true`. These events merge persons or update person or group properties, which PostHog skips when person processing is off.

PostHog drops `$create_alias` and `$groupidentify` events that have person processing off and records an `invalid_event_when_process_person_profile_is_false` ingestion warning in your project. Capture accepts these events before the drop, so `on_error` doesn't see it.

The SDK therefore keeps the request context from turning person processing off for them, except on an event whose distinct ID the SDK generated. This covers `alias`, `group_identify`, and any event with one of these names that you build and capture yourself.

- **Event values.** The event's own value wins over every layer. An event that sets the `process_person_profile` option keeps it. An event that sets only the legacy `$process_person_profile` property sends that value as the option. A `null` value counts as unset.
- **The default.** Otherwise, when the SDK didn't generate the event's distinct ID, the SDK sets the option to `true` before it applies the request context, so a context option or property of `false` doesn't reach the event.
- **Generated distinct IDs.** An event whose distinct ID the SDK generated gets no default and stays personless. That's an event from `Event::new_anon`, from `Event::new` with a blank ID, or from `RequestContext::event` without a context distinct ID. A `$process_person_profile` property you set on such an event wins over the personless `false`.
- **`before_send`.** The hook sees the resolved value as the option, without a `$process_person_profile` property next to it. It can change the option, and the SDK sends what the hook returns. A hook that removes the option sends the event without one.
- **Hooks that build or rename events.** The SDK checks the event name when you capture the event. A `before_send` hook that builds an event with one of these names, or renames an event to one, must set the option itself.
- **Contexts you apply yourself.** When you apply a request context yourself with `RequestContext::event` or `RequestContext::apply_to_event`, set the `process_person_profile` option on an identify-style event afterwards, not the legacy `$process_person_profile` property. The SDK ignores a legacy property set after that, because the default option already won.
- **Keeping one personless.** To send an identify-style event without person processing, set the `process_person_profile` option to `false` on the event or in `before_send`, or turn off person processing for the project in PostHog.

To set options on the events that `alias` and `group_identify` send, build them with `Event::alias` and `Event::group_identify`, then capture them:

```rust
let mut event = posthog::Event::alias("anon-abc123", "user-42")?;
event.insert_option("disable_skew_correction", true)?;
client.capture(event);
```

Both constructors return `Error::InvalidArgument` for a blank ID, group type, or group key. `client.alias` and `client.group_identify` still log a warning and drop the event.

In 0.x.x, a context that turned person processing off reached `alias` and `group_identify`, and PostHog dropped their events.

### Check person processing

- **Anonymous events.** `Event::new_anon` sets the `process_person_profile` option to `false`. To process person profiles for an anonymous event, set the option with `insert_option("process_person_profile", true)`. A `null` or removed option counts as unset, so the SDK sets `false` again before `before_send`.
- **Context options.** A context `process_person_profile` option replaces the `false` that `Event::new_anon` sets.
- **Identify-style events.** An [identify-style event](#check-identify-style-events) gets `process_person_profile: true` before the context applies, unless it sets its own value or the SDK generated its distinct ID.
- **Groups.** Adding a group no longer turns on person processing. In 0.x.x, `Event::add_group` and `CaptureExceptionOptions::group` turned person processing on. In 1.x.x they leave it unchanged.
- **Events from `Event::new`.** They still process person profiles by default.
- **Anonymous events with groups.** An anonymous event with groups, from `Event::new_anon` or an exception without a distinct ID, now stays personless. PostHog attaches groups for group analytics only to events that process person profiles. Set `insert_option("process_person_profile", true)` on those events if you need them in group analytics.

### Check option values

The SDK sends option values unchanged, and PostHog validates them:

- **Unknown keys.** PostHog ignores keys it doesn't know.
- **Common forms.** PostHog converts common forms of a known key's value. For a boolean, those include `"yes"`, `"off"`, `"false"`, and `0`.
- **Values PostHog can't read.** PostHog drops the event if a known key has a value it can't read. That drop reaches `on_error` as a per-event `drop` with the detail `invalid_options`.
- **0.x.x with `capture-v1`.** The SDK removed a value it couldn't read and sent the event without it.
- **0.x.x without `capture-v1`.** It used the V0 endpoint, which ignored a `process_person_profile` option of `"false"`. In 1.x.x that string turns person processing off. The V0 endpoint also kept an event with a value it couldn't read and ignored the value.

### Check property precedence

An event's properties and options come from three layers. For the same key, a layer earlier in this list wins, except for `$lib`, `$lib_version`, and its version parts, which follow the rule in [Check SDK identity](#check-sdk-identity):

1. Event values: what you set on the event with `insert_prop` and `insert_option`, or on `CaptureExceptionOptions`. For `$feature_flag_called` events, a per-call `disable_geoip` in `EvaluateFlagsOptions` counts as an event value.
2. Request context: properties and options from `RequestContext`. They fill only the keys the event leaves unset.
3. SDK values: `$lib`, `$lib_version` and its three version parts, `$os`, `$os_version`, and `$is_server` and `$geoip_disable` from the client settings, plus the `process_person_profile` option that `Event::new_anon` sets to `false`.

`before_send` runs after all three and can change any value. The SDK has no client-wide layer for properties or options.

A property set to `null` counts as set, so the request context doesn't fill it. A `null` option counts as unset.

- **System properties.** The SDK adds `$os`, `$os_version`, `$is_server`, and `$geoip_disable` only when the event doesn't already have the key, and it adds them before `before_send` runs. The hook sees them and can change or remove them, and the SDK doesn't add them again after the hook.
- **System properties in 0.x.x.** The default 0.x.x path (V0) also added them before the hook. The 0.x.x `capture-v1` path added them while building the request, after the hook, so a hook couldn't see `$os` or remove any of them.
- **`$lib` and `$lib_version`.** The SDK adds them before `before_send` runs, as a pair, as described in [Check SDK identity](#check-sdk-identity). The 0.x.x `capture-v1` path never added them.
- **Feature flag properties.** `Event::with_flags` keeps a `$feature/<key>` or `$active_feature_flags` property that the event already has. In 0.x.x, the flag values replaced it.
- **Groups.** Groups from `Event::add_group` merge into a `$groups` property key by key, and the added group wins for the same group type. In 0.x.x, they replaced the whole `$groups` property.
- **Groups in `before_send`.** The SDK merges those groups into `$groups` before `before_send` runs, so the hook sees them and can change or remove them. The default 0.x.x path (V0) also merged them before the hook. The 0.x.x `capture-v1` path merged them while building the request, after the hook. A group the hook adds with `add_group` is still sent.
- **Merged maps.** A `$set`, `$set_once`, `$groups`, or `$group_set` from the request context fills in only the keys the event's own value lacks. For example, a context `$set` of `{"plan": "free", "source": "web"}` and an event `$set` of `{"plan": "pro"}` send `{"plan": "pro", "source": "web"}`. In 0.x.x, the context value was used only when the event had none of that property. A `$set` in the request context sets those person properties on every event it reaches.
- **Minimized flag events.** For a minimized `$feature_flag_called` event, the SDK trims the properties to its allowlist before `before_send` runs, so a property the hook adds is sent. In 0.x.x the trim ran after the hook and removed it.
- **Flag events.** `$feature_flag_called` events now take a request context value over the client's `$is_server` and `$geoip_disable` defaults, as other events do. A per-call `disable_geoip` wins over both, and `Some(false)` sends `$geoip_disable: false`. In 0.x.x, `Some(false)` sent nothing, so a request context `$geoip_disable` applied.
- **The `disable_geoip` default.** `disable_geoip` defaults to `false`, as in 0.x.x and unlike PostHog's other server SDKs, so PostHog adds GeoIP properties based on the IP address of your server. To turn this off, call `.disable_geoip(true)` on `ClientOptionsBuilder`.

### Check request context

The request context works only with the async client. Code inside `RequestContext::scope` sends the context with these calls:

- **Capture calls.** `capture`, `capture_batch`, `capture_immediate`, `capture_batch_immediate`, the `capture_ai` methods, and `capture_exception`.
- **Identity calls.** `alias` and `group_identify`.
- **Flag snapshots.** A flag snapshot from `evaluate_flags` inside the scope keeps the context for its `$feature_flag_called` events, even after the scope ends.

An event you build with `Event::new` or `Event::new_anon` keeps its own distinct ID. An exception without a distinct ID takes the context distinct ID.

MCP events from the `mcp` feature take the request context too:

- **Over HTTP.** `rmcp` runs each request handler in a task of its own, so over HTTP the SDK reads the `RequestContext` that `PostHogLayer` or your middleware placed in the request extensions.
- **A scoped context.** A context scoped around the code that sends the event wins over the one in the request extensions.
- **The distinct ID.** The identity from the `McpOptions` `identify` callback stays the distinct ID.
- **Anonymous MCP events.** An anonymous MCP event keeps its MCP session ID as the distinct ID and keeps `process_person_profile: false` over a context option.
- **The hook.** The `McpOptions` hook sees the context values.

The blocking client has no scoped context, and `RequestContext::current()` returns `None`. Apply a context to each event yourself with `RequestContext::event` or `RequestContext::apply_to_event`, and use `evaluate_flags_with_context` for flags.

### Check SDK identity

Each event's `$lib` and `$lib_version` properties name the library that produced it. The SDK sends them in `properties` as they are when the event is sent, and PostHog stores them.

The SDK also sends `posthog-rs/<version>` in the `posthog-sdk-info` request header. PostHog uses the header only to fill in what an event leaves unset: both properties when `$lib` isn't set, and the version when `$lib` is `posthog-rs` and `$lib_version` isn't set.

You can set them on the event or in the request context. Like any other property, each one comes from the most specific place that sets it, so set both in the same place.

The SDK fills these properties after the event and request context values, and before `before_send` runs.

A `$lib` counts as set only when it's a non-empty string, so a missing, `null`, empty, or non-string `$lib` isn't set. The same goes for `$lib_version`.

| The event has | The SDK sends |
| --- | --- |
| No `$lib` | `$lib` as `posthog-rs` and `$lib_version` as the crate version |
| A `$lib` of `posthog-rs` and no `$lib_version` | The crate version in `$lib_version` |
| A `$lib` that names another library | Both properties as they are |

A `$lib_version` set without `$lib` is replaced.

The SDK sends another library's `$lib` and `$lib_version` as they are, even if `$lib_version` is missing. Set both when you name another library.

`before_send` sees the result and can change either property. The SDK doesn't fill them again after the hook.

PostHog still uses the header as described above, so an event the hook sends without `$lib` is stored as `posthog-rs`. To label an event as another library, set both `$lib` and `$lib_version` in the hook.

When the SDK adds its own `$lib_version`, it also adds `$lib_version__major`, `$lib_version__minor`, and `$lib_version__patch` as numbers.

It never adds them to another library's `$lib`. A minimized `$feature_flag_called` event keeps `$lib` and `$lib_version` but not the three version parts.

The two 0.x.x paths filled these properties in other ways:

- **The default 0.x.x path (V0).** It also added these properties before `before_send`, but one key at a time, so a `$lib` you set on its own got the SDK's `$lib_version` and version parts. In 1.x.x it doesn't. That path also kept a `null`, empty, or non-string `$lib` as sent, where 1.x.x replaces it.
- **The 0.x.x `capture-v1` path.** It didn't add these properties, so PostHog set them from the header.

The `mcp` feature sets `$lib` to `posthog-rs-mcp` and `$lib_version` to the crate version on its events, and PostHog stores both. 0.x.x without `capture-v1` also stored `posthog-rs-mcp`, and added the three version parts, which 1.x.x doesn't send on MCP events.

To label MCP events as another library, set `$lib` and `$lib_version` in the `McpOptions` hook or the client's `before_send`. Removing `$lib` in either hook makes the event report `posthog-rs`.

`$mcp_interface` is `mcp` on every MCP event.

### Check dropped events

- **Empty names.** The SDK sends an event with an empty name, and PostHog drops it with the per-event result `missing_event_name`. PostHog's other server SDKs drop it before sending.
- **Long names.** PostHog drops an event whose name is over 200 UTF-8 bytes, with the per-event result `event_name_too_long`. Legacy capture had no limit on event name length.
- **`NaN` and infinite numbers.** The SDK sends a `NaN` or infinite number in a property as `null`, as in 0.x.x, and keeps the event. A `null` in `$set` clears that person property.

Each per-event result reaches `on_error`, or the `CaptureSummary` of an immediate method.

The SDK sends `$session_id` and `$window_id` as top-level fields of the event, because capture reads them there and requires strings. The value's type decides what the SDK sends:

- **A string.** The SDK sends it unchanged, including an empty string.
- **`null`.** It counts as unset, and the SDK drops it without a warning.
- **Any other value.** The SDK drops it and logs a warning for each drop. The warning names the key and the value's type, never the value. In 0.x.x without `capture-v1`, the SDK sent any value as a property.

### Check batch sizes

The regular `capture` and `capture_batch` methods remain fire-and-forget. They enqueue events for the background worker, which batches and retries delivery.

`capture_immediate` and `capture_batch_immediate` still bypass the queue and return a `CaptureSummary` after reaching a terminal result.

- **Analytics batches.** The background worker now splits each analytics batch into requests of at most 5 MiB of serialized events. A batch of large events no longer exceeds capture's 10 MiB request limit and fails whole.
- **Large events.** A single event larger than 5 MiB is sent on its own.
- **`before_send`.** It still runs when a batch is sent.
- **Immediate batches.** A batch passed to `capture_batch_immediate` is still sent as one request.

### Check compression

`CaptureCompression::Gzip`, `Deflate`, `Br`, and `Zstd` now all apply their corresponding `Content-Encoding`. In older default V0 builds, only gzip was supported, and selecting another variant could send an uncompressed body.

Check any proxy or WAF in front of PostHog before you enable Brotli or Zstandard.

### Check exceptions

- **Debug images.** `$debug_images` lists only images with a debug ID that a sent stack frame points into. In 0.x.x it could also list an image that only a trimmed frame pointed into.
- **Trace IDs.** A `$trace_id` or `$span_id` you set in the exception's properties now wins over the trace context provider. Setting either one stops the SDK adding both provider IDs.
- **The `trace_context` option.** `CaptureExceptionOptions::trace_context` still wins over both.

### Update error handling

The capture v1 endpoint returns a result for each event.

`CaptureSummary::not_persisted()` and `CaptureSummary::all_persisted()` now use those per-event results. In 0.x.x, the V0 path reported a successful `2xx` as fully persisted without per-event confirmation.

If your application advances durable state after `capture_immediate`, check both that `submitted()` equals the number of intended events and that `all_persisted()` is true. Disabled clients and fully `before_send`-filtered batches submit no events but still report `all_persisted()` as true.

- **Billing limits.** HTTP `402` means the whole request is over the billing limit.
- **Product quotas.** A product quota drops single events inside a `200`, as a per-event `drop` such as `llm_events_over_quota`.
- **Logs without `on_error`.** Without a hook the SDK logs one aggregate warning per batch with drop and retry totals.
- **Drop and retry reasons.** To log every drop and retry reason, call `CaptureFailure::verdict_summary()` in an `on_error` hook. It returns one line such as `drop/invalid_options=2, retry/not_persisted=1`.
- **Captures inside `on_error`.** The hook must not call back into the SDK (`capture`, `flush`, or `shutdown`), because an event sent while handling a failure can fail again and loop.
- **Immediate methods.** They never call `on_error`.
- **Drops that skip `on_error`.** Queue-full and `before_send` drops don't reach `on_error`.

During shutdown, the SDK makes one final attempt per batch:

- **A retryable failure.** If that attempt gets a network error, a retryable HTTP status, or a per-event `retry` result while the batch still has attempts left, the SDK drops those events instead of retrying them.
- **The timeout.** The SDK also drops any events left when `shutdown_timeout_ms` runs out.
- **How these drops are reported.** They're logged with a warning that names the endpoint, and they don't reach `on_error`, as in 0.x.x.
- **A batch out of attempts.** A batch that has used all its `max_capture_attempts` fails as usual and reaches `on_error`.

### Check retries

The SDK retries transient request failures. From a partial response, it retries only the events with retryable results.

| Behavior | 0.x.x (V0) | 1.x.x |
| --- | --- | --- |
| Capture attempts (`max_capture_attempts`) | 3 by default | 4 by default |
| First retry wait (`retry_initial_backoff_ms`) | 200 ms | 100 ms |
| Statuses retried | `429` only with `Retry-After`, never `404` | Network errors and HTTP `404`, `408`, `429`, `500`, `502`, `503`, and `504` |
| First remote `/flags` retry wait | 200 ms by default | 300 ms |

In 0.x.x, the V0 path retried a `429` only when it had a `Retry-After` header, and never retried a `404`. In 1.x.x, every other status is terminal.

A `404` that lasts through every attempt can mean the PostHog deployment has no capture v1 route, for example an older self-hosted install (see [Prepare your proxy](#prepare-your-proxy)).

In 0.x.x, remote `/flags` retries used the capture retry options, with a first wait of 200 ms by default. In 1.x.x, they wait 300 ms before the first retry and double up to 30 s. They no longer use the capture retry options.

In 1.x.x each later wait doubles up to `retry_max_backoff_ms`, 30 s by default. Each capture retry wait varies at random by up to 25% either way, so clients that fail together don't retry together.

A `Retry-After` header is still the minimum wait, capped at `retry_max_backoff_ms` as in 0.x.x. The SDK still honors it for retryable statuses and `retry` results.

The capture retry options apply to `capture` and `capture_ai`. `feature_flags_request_max_retries` still sets how many `/flags` retries the SDK makes.

Capture itself doesn't return `429`, but a proxy in front of it can.

## Follow ups

### Rename the crate

1.x.x publishes the SDK as `posthog`. New applications should depend on `posthog = "1"` and import `posthog`.

Existing applications can use `posthog-rs = "1"` and keep their `posthog_rs` imports. That package forwards every feature and re-exports the same implementation, types, and global state.

The tracing target is `posthog` in both crates (see [Update log filters](#update-log-filters)).

### Move to event options

Capture v1 reads per-event processing controls, such as person profile processing, from an `options` map that is sent next to `properties`. Set them with `Event::insert_option`, or with `CaptureExceptionOptions::option` for exceptions:

```rust
let mut event = posthog::Event::new("signed_up", "user-123");
event.insert_option("process_person_profile", false)?;
client.capture(event);
```

- **Both lanes.** Options work the same on `capture` and `capture_ai`.
- **Every event in a request.** To set an option on every event in a request, use `RequestContext::option` (see [Check request context](#check-request-context)). An option the event sets, other than `null`, wins over it. A context `process_person_profile` doesn't reach [identify-style events](#check-identify-style-events) whose distinct ID the SDK didn't generate.
- **Keys and values.** Keys are strings and values are any JSON value.
- **New options.** The SDK sends options as given, so a new option that PostHog adds needs no SDK upgrade. PostHog validates them (see [Check option values](#check-option-values)).

Each option replaces a legacy option property:

| Option | Replaces |
| --- | --- |
| `process_person_profile` | `$process_person_profile` |
| `cookieless_mode` | `$cookieless_mode` |
| `disable_skew_correction` | `$ignore_sent_at` |
| `product_tour_id` | `$product_tour_id` |

Legacy option properties still work, but an option overrides its legacy property. Use options.

For the changes every capture v1 SDK shares, such as event options, limits, and per-event results, see the [capture v1 API reference](https://posthog.com/docs/api/capture-v1).

### Add MCP event options

MCP events from the `mcp` feature take options from two places:

- **The callback.** The `McpOptions` `event_options` callback runs for each request.
- **The manual API.** `ToolCallRequest::event_option` sets an option on one call.
- **Precedence.** The call's own option wins over the callback's, and both win over the request context.
- **Anonymous events.** An anonymous MCP event keeps `process_person_profile: false`.
- **Hooks.** The `McpOptions` hook and then the client's `before_send` see the result and can change it.

Set the callback in `McpOptionsBuilder`, or call `event_option` on a `ToolCallRequest`:

```rust
use posthog::mcp::{McpOptionsBuilder, ToolCallRequest};

let options = McpOptionsBuilder::default()
    .event_options(|_request| {
        let mut options = serde_json::Map::new();
        options.insert("disable_skew_correction".into(), true.into());
        Some(options)
    })
    .build()?;
let call = ToolCallRequest::new("search_docs", None).event_option("product_tour_id", "onboarding")?;
```

### Send AI events with capture_ai

Use the `capture_ai` family for LLM analytics events. Those are `$ai_generation`, `$ai_span`, `$ai_trace`, `$ai_embedding`, and the other `$ai_*` names PostHog's LLM analytics product defines.

If you send `$ai_` events with `capture`, switch them to `capture_ai`:

```rust
let event = posthog::Event::new("$ai_generation", "user-123");

// 0.x.x
client.capture(event);

// 1.x.x
client.capture_ai(event);
```

For now the capture v1 analytics endpoint still accepts `$ai_` events sent through `capture`, but this is temporary.

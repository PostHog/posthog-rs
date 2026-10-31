# Migrating from posthog-rs 0.x to 1.0

Version 1.0 makes the V1 analytics endpoint the SDK's only capture path and removes APIs that were already deprecated in 0.x. This guide lists the code changes 1.0 needs and the behavior that differs from 0.x.

<!-- TODO(capture-v1): when the public capture v1 SDK upgrade guide is published (PostHog/posthog.com#21386), link it here for details on options, limits and per-event results. -->

## Cargo features

Remove `capture-v1` from your dependency features. Capture no longer needs a feature flag.

```toml
# 0.x while opting into V1 capture
posthog-rs = { version = "0.27", features = ["capture-v1"] }

# 1.0
posthog-rs = "1"
```

The default features are the async client, error tracking, and TLS. Use `default-features = false` for the blocking client and explicitly enable a TLS feature when sending to an HTTPS endpoint.

```toml
# Async client with error tracking and TLS
posthog-rs = "1"

# Blocking client with TLS
posthog-rs = { version = "1", default-features = false, features = ["tls"] }
```

TLS was previously enabled even with `default-features = false`. In 1.0, choose `tls` to use reqwest's default Rustls provider, or choose `tls-no-provider` when the application installs a process-level Rustls `CryptoProvider`. The latter avoids pulling in `aws-lc-rs` from this SDK, but constructing a client before installing a provider will panic. Cargo features are additive, so `tls-no-provider` only avoids the built-in provider when no dependency enables `tls`.

The V0 capture implementation and its `/i/v0/e/` and batch plumbing have been removed. `Endpoint::Batch` no longer exists, and `Endpoint::Capture` now resolves to `/i/v1/analytics/events`. `Endpoint` is now non-exhaustive and has a new `CaptureAi` variant, so a `match` on it needs a wildcard arm.

## Capture behavior

The regular `capture` and `capture_batch` methods remain fire-and-forget. They enqueue events for the background worker, which batches and retries delivery. `capture_immediate` and `capture_batch_immediate` still bypass the queue and return a `CaptureSummary` after reaching a terminal result. The background worker now also splits each analytics batch into requests of at most 5 MiB of serialized events, so a batch of large events no longer exceeds capture's 10 MiB request limit and fails whole. A single event larger than 5 MiB is sent on its own. `before_send` still runs when a batch is sent, and a batch passed to `capture_batch_immediate` is still sent as one request.

All event-producing SDK paths now use the same capture endpoint, including error tracking, `$feature_flag_called` events, historical migration, `before_send`, and terminal failures reported through `on_error`.

### Reverse proxies

If you send events through your own reverse proxy, update it before you upgrade. Forward `/i/v1/analytics/events` and `/i/v1/ai/events` to PostHog. Pass `Authorization` (the project token, sent as `Bearer <token>`), every `PostHog-*` header, `Content-Type`, `Content-Encoding` and `User-Agent` through unchanged: PostHog answers `401` without `Authorization` and `400` without the others, and the batch fails. Accept request bodies up to 20 MB: AI requests are often larger than 1 MB, the default limit in nginx and ingress-nginx, and a `413` from the proxy fails the batch. PostHog's managed reverse proxy already does this. See the [proxy guides](https://posthog.com/docs/advanced/proxy).

### Event IDs

The SDK gives every event a UUIDv7 `uuid` and every request a UUIDv7 request ID. If you set your own with `Event::set_uuid`, keep it unique. When two events in one request share a `uuid`, PostHog keeps the first and drops the later copies. The SDK reports each dropped copy through `on_error` and `CaptureSummary` with the reason `duplicate_event_uuid`. An entry's `EventResult::duplicates_dropped` counts the copies dropped with it. If the kept copy comes back `retry` in the background worker, those dropped copies are not reported; PostHog still raises a `duplicate_event_uuid` ingestion warning in your project. Older PostHog versions reject the whole request with HTTP 400 `duplicate_event_uuid` instead.

### SDK identity

PostHog sets `$lib` and `$lib_version` on every event from the `posthog-sdk-info` request header, which is always `posthog-rs/<version>`. The SDK no longer adds them to properties. If you set them with `insert_prop` or in `before_send`, the SDK removes them before it sends the event, so stored events always report `posthog-rs`. `before_send` still sees the value you set. In 0.x without `capture-v1`, PostHog stored a `$lib` or `$lib_version` you set. The 0.x `capture-v1` path already sent the header, so PostHog replaced your value.

The SDK no longer sends `$lib_version__major`, `$lib_version__minor` or `$lib_version__patch`. Only the 0.x V0 path sent them. Filter on `$lib_version` instead.

### AI events

Use the new `capture_ai` family for LLM analytics events (`$ai_generation`, `$ai_span`, `$ai_trace`, `$ai_embedding`, and the other `$ai_*` names PostHog's LLM analytics product defines). `capture_ai`, `capture_ai_batch`, `capture_ai_immediate`, `capture_ai_batch_immediate`, and the global `posthog_rs::capture_ai` mirror their analytics counterparts but post to `/i/v1/ai/events` on their own background lane, batched by size. The SDK drops an event over the backend's 8 MiB per-event limit locally, on the background lane and in the immediate variants. The immediate variants return that drop in the `CaptureSummary` with the detail `ai_event_too_big`. An event too large for the AI endpoint's request size limit gets HTTP 413 instead: `capture_ai_batch_immediate` returns `Error::BadRequest` and does not send the rest of the batch. The lane has its own options: `capture_ai_compression` (unset sends AI bodies uncompressed; set `CaptureCompression::Zstd`, the best fit for large JSON) and `capture_ai_max_queue_size` (default 1000).

`capture` never reroutes by event name. Send AI events with `capture_ai`. PostHog owns the `$ai_` prefix, so don't use it for your own events. For now the V1 analytics endpoint still accepts `$ai_` events sent through `capture`, but this is temporary, and large AI events always need `capture_ai`.

The backend decides which names belong on the AI lane and reports a misrouted or oversize event as a per-event `drop` inside a `200`. That verdict reaches an `on_error` hook (`CaptureFailure::endpoint()` says which lane) or the returned `CaptureSummary` for the immediate variants; without a hook the SDK logs one aggregate warning per batch with drop and retry totals. To log every drop and retry reason, call `CaptureFailure::verdict_summary()` in an `on_error` hook; it returns one line such as `drop/invalid_options=2, retry/not_persisted=1`.

### Event uuid, timestamp and distinct ID

- The SDK assigns an event's uuid when you hand the event to `capture` or `capture_immediate`, not in `Event::new`. Capturing a cloned or reused `Event` twice now sends two uuids. In 0.x both copies shared one uuid, and capture V1 rejects a request that contains a uuid twice. A uuid set with `Event::set_uuid` is still sent as given on every capture.
- `Event::set_timestamp` accepts timestamps in the future. In 0.x it returned `Error::InvalidTimestamp` for a timestamp more than 1 s ahead. PostHog clamps far-future timestamps, as it does for every other SDK. The method keeps its `Result` return type.
- `Event::new` with an empty or whitespace-only distinct ID creates an anonymous event, as `Event::new_anon` does. In 0.x it sent the empty ID, and PostHog dropped the event.
- PostHog drops an event whose distinct ID is over 200 UTF-8 bytes, with the per-event result `distinct_id_too_large`, which reaches `on_error` (or the `CaptureSummary` of an immediate method). 0.x sent events to legacy capture, which shortened the ID to 200 characters and kept the event.
- PostHog drops an event whose name is over 200 UTF-8 bytes, with the per-event result `event_name_too_long`, reported the same way. This is new: legacy capture had no limit on event name length.
- `group_identify` sends `$<group_type>_<group_key>` as the distinct ID. If that is over 200 bytes, PostHog drops the event. To set a group with a longer key, capture a `$groupidentify` event yourself with `Event::new("$groupidentify", <your distinct ID>)` and the `$group_type`, `$group_key` and `$group_set` properties.

### Event options

Capture V1 reads per-event processing controls, such as person profile processing, from an `options` map that is sent next to `properties`. Set them with `Event::insert_option`, or with `CaptureExceptionOptions::option` for exceptions. They work the same on `capture` and `capture_ai`. To set an option on every event in a request, use `RequestContext::option`. An option the event sets, other than `null`, wins over it.

```rust
let mut event = posthog_rs::Event::new("signed_up", "user-123");
event.insert_option("process_person_profile", false)?;
client.capture(event);
```

Keys are strings and values are any JSON value. The SDK sends options as given, so a new option that PostHog adds needs no SDK upgrade. PostHog validates them: it ignores keys it does not know, converts common forms of a known key's value (for a boolean, `"yes"`, `"off"` or `0`), and drops the event if a known key has a value it cannot read. That drop reaches `on_error` as a per-event `drop` with the detail `invalid_options`. `before_send` hooks can read and change options with `Event::options`, `insert_option`, and `remove_option`.

Review your `before_send` hooks for the new event shape. To change an option, use `insert_option`: a legacy property that the hook adds with `insert_prop` does not replace an option that is already set. A hook that returns a new `Event` must copy the options of the event it received, or the event is sent without them. That includes the `process_person_profile` option that `Event::new_anon` sets to `false`, so PostHog would create a person for the generated ID.

The legacy option properties, the `$`-prefixed properties legacy capture reads as settings, still work. The SDK moves each one into its option and removes it from `properties`:

| Legacy option property | Option |
| --- | --- |
| `$process_person_profile` | `process_person_profile` |
| `$cookieless_mode` | `cookieless_mode` |
| `$ignore_sent_at` | `disable_skew_correction` |
| `$product_tour_id` | `product_tour_id` |

When both are set, the option wins. An option set to `null` counts as not set, so the legacy property applies.

One behavior differs from the 0.x `capture-v1` feature:

- The SDK no longer converts legacy property values to the type PostHog expects. A value PostHog cannot read, such as `"$process_person_profile": "maybe"`, used to be removed so the event was sent without it. Now PostHog drops the event as `invalid_options`. Values PostHog accepts, such as `"false"` or `0`, behave as before.

Two behaviors differ from every 0.x build:

- `Event::new_anon` sets the `process_person_profile` option to `false` instead of the legacy property. Because an option wins over its legacy property, `insert_prop("$process_person_profile", true)` no longer turns processing back on for an anonymous event. Use `insert_option("process_person_profile", true)`. A `null` or removed option counts as unset, so the SDK sets `false` again before `before_send`.
- Adding a group no longer turns on person processing. In 0.x, `Event::add_group` and `CaptureExceptionOptions::group` set `$process_person_profile` to `true`. In 1.0 they leave it unchanged. An event from `Event::new` still processes person profiles by default. An anonymous event with groups (from `Event::new_anon`, or an exception without a distinct ID) now stays personless. PostHog attaches groups for group analytics only to events that process person profiles, so set `insert_option("process_person_profile", true)` on those events if you need them in group analytics.

Values the SDK adds, and the values you set:

- The SDK adds `$os`, `$os_version`, `$is_server` and `$geoip_disable` only when the event does not already have the key, and it adds them before `before_send` runs. The hook sees them and can change or remove them, and the SDK does not add them again after the hook. The default 0.x path (V0) also added them before the hook. The 0.x `capture-v1` path added them while building the request, after the hook, so a hook could not see `$os` or remove any of them.
- `Event::with_flags` keeps a `$feature/<key>` or `$active_feature_flags` property that the event already has. In 0.x, the flag values replaced it.
- Groups from `Event::add_group` merge into a `$groups` property key by key, and the added group wins for the same group type. In 0.x, they replaced the whole `$groups` property.
- A `$set`, `$set_once`, `$groups` or `$group_set` from the request context fills in only the keys the event's own value lacks. For example, a context `$set` of `{"plan": "free", "source": "web"}` and an event `$set` of `{"plan": "pro"}` send `{"plan": "pro", "source": "web"}`. In 0.x, the context value was used only when the event had none of that property.
- The SDK merges those groups into `$groups` before `before_send` runs, so the hook sees them and can change or remove them. The default 0.x path (V0) also merged them before the hook. The 0.x `capture-v1` path merged them while building the request, after the hook. A group the hook adds with `add_group` is still sent.
- For a minimized `$feature_flag_called` event, the SDK trims the properties to its allowlist before `before_send` runs, so a property the hook adds is sent. In 0.x the trim ran after the hook and removed it.

One behavior differs from 0.x without `capture-v1`, which used the V0 endpoint:

- PostHog now reads common forms of an option value, so `"$process_person_profile": "false"` turns person processing off. The V0 endpoint ignored that string. A value PostHog cannot read now drops the event, where the V0 endpoint kept the event and ignored the value.

### Session and window IDs

The SDK sends `$session_id` and `$window_id` as top-level fields of the event, because capture reads them there and requires strings. A string value, including an empty string, is sent unchanged. Any other value is dropped, and the SDK logs a warning for each drop that names the key and the value's type, never the value. A `null` value counts as unset and drops without a warning. In 0.x without `capture-v1`, the SDK sent any value as a property.

### Compression

`CaptureCompression::Gzip`, `Deflate`, `Br`, and `Zstd` now all apply their corresponding `Content-Encoding`. In older default V0 builds, only gzip was supported and selecting another variant could send an uncompressed body. Check any proxy or WAF in front of PostHog before enabling Brotli or Zstandard.

Brotli and Zstandard support is always compiled in. `zstd-sys` builds C code with the `cc` crate, so the build needs a C compiler. Every 0.x build already needed one for `aws-lc-sys`, the TLS crypto library.

### Retry and persistence results

The V1 endpoint returns a result for each event. The SDK retries transient request failures and only the events with retryable results from a partial response. Each capture retry wait varies at random by up to 25% either way, so clients that fail together do not retry together; a `Retry-After` header is still the minimum wait.

Capture now makes 4 attempts by default (`max_capture_attempts`, was 3). The first retry waits 100 ms (`retry_initial_backoff_ms`, was 200 ms), and each later wait doubles up to `retry_max_backoff_ms`. These options apply to `capture` and `capture_ai`. Remote `/flags` retries no longer use them. They wait 300 ms before the first retry (was 200 ms by default) and double up to 30 s. `feature_flags_request_max_retries` still sets how many `/flags` retries the SDK makes.

`CaptureSummary::not_persisted()` and `CaptureSummary::all_persisted()` now use those per-event results. In the V0 path they reported a successful `2xx` as fully persisted without per-event confirmation. Applications that advance durable state after `capture_immediate` should check both that `submitted()` equals the number of intended events and that `all_persisted()` is true, because disabled clients and fully `before_send`-filtered batches submit no events but still report `all_persisted()` as true.

The SDK retries network errors and HTTP 404, 408, 429, 500, 502, 503 and 504. Every other status is terminal. The 0.x V0 path retried a 429 only when it had a `Retry-After` header, and never retried a 404. Capture itself does not return 429, but a proxy in front of it can. A 404 that lasts through every attempt can mean the PostHog deployment has no capture V1 route, for example an older self-hosted install. HTTP 402 means the whole request is over the billing limit. A product quota drops single events inside a `200`, as a per-event `drop` such as `llm_events_over_quota`. `Retry-After` is still honored for retryable statuses and `retry` results.

Without an `on_error` hook, a request that fails for good logs `posthog-rs: dropping <n> event(s) for <endpoint>: <error>`, as in 0.x. The error text can include PostHog's response body. Register an `on_error` hook to choose what to log. The hook must not call back into the SDK (`capture`, `flush` or `shutdown`), because an event sent while handling a failure can fail again and loop. The immediate methods never call `on_error`.

During shutdown, the SDK makes one final attempt per batch. If that attempt gets a retryable failure (a network error, a retryable HTTP status, or a per-event `retry` result) while the batch still has attempts left, the SDK drops those events instead of retrying them. It also drops any events left when `shutdown_timeout_ms` runs out. These drops are logged with a warning that names the endpoint, and they do not reach `on_error`, as in 0.x. A batch that has used all its `max_capture_attempts` fails as usual and reaches `on_error`. Queue-full and `before_send` drops do not reach `on_error` either.

The capture and `/flags` HTTP clients the SDK builds follow a redirect only when it is a 307 or 308 to the same scheme, host and port, at most 5 times in a row. Any other redirect fails the request, so the SDK never sends the request body to another host. 0.x followed up to 10 redirects of any kind to any host, and local evaluation polling still does. A client you pass with `http_client` or `blocking_http_client` keeps its own redirect policy.

### Renamed capture response type

Rename `V1ErrorResponse` to `CaptureErrorResponse`:

```rust
// 0.x with capture-v1
let response: Option<&posthog_rs::V1ErrorResponse> = failure.error_response();

// 1.0
let response: Option<&posthog_rs::CaptureErrorResponse> = failure.error_response();
```

`CaptureResponse` and `EventResult` are now non-exhaustive, so you cannot build them with a struct literal.

## Error tracking

`$debug_images` lists only images with a debug ID that a sent stack frame points into. In 0.x it could also list an image that only a trimmed frame pointed into.

A `$trace_id` or `$span_id` you set in the exception's properties now wins over the trace context provider, and setting either one stops the SDK adding both provider IDs. `CaptureExceptionOptions::trace_context` still wins over both.

## Feature flags

The deprecated single-flag methods have been removed:

- `is_feature_enabled`
- `get_feature_flag`
- `get_feature_flag_payload`
- `get_feature_flags`

Call `evaluate_flags` once and read from the returned snapshot instead:

```rust
use posthog_rs::EvaluateFlagsOptions;

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

These feature flag types are non-exhaustive too: `FeatureFlagsResponse`, `FlagDetail`, `FlagMetadata`, `FlagReason` and `LocalEvaluationResponse`. A `match` on the `FeatureFlagsResponse` enum needs a wildcard arm. The structs no longer accept struct literals. Build a `LocalEvaluationResponse` with `LocalEvaluationResponse::new` and assign its public fields.

## Local evaluation credentials

Use `secret_key` terminology throughout configuration. The builder's deprecated `personal_api_key` alias has been removed, and `LocalEvaluationConfig::personal_api_key` is now `secret_key`.

```rust
let options = posthog_rs::ClientOptionsBuilder::default()
    .api_key("phc_project_token".to_string())
    .secret_key("phs_project_secret")
    .enable_local_evaluation(true)
    .build()?;
```

`secret_key` accepts either a project secret key (`phs_...`) or a personal API key (`phx_...`). Do not send this key as an event property.

A personal API key passed as the project `api_key` now disables the client and logs one error, as an empty key does. PostHog never accepts a personal key for capture, so every request with it failed with a 401. An empty key now logs at the error level instead of warning. A client built with `disabled(true)` logs nothing about its key.

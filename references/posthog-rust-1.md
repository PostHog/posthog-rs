---
title: "PostHog Rust SDK 1.x (experimental)"
sidebar: Docs
showTitle: true
contentMaxWidthClass: max-w-full
platformLogo: rust
tableOfContents:
  - value: "Getting started"
    url: getting-started
    depth: 0
  - value: "Setup"
    url: setup
    depth: 0
  - value: "Capture"
    url: capture
    depth: 0
  - value: "Alias"
    url: alias
    depth: 0
  - value: "Capture AI"
    url: capture-ai
    depth: 0
  - value: "Capture exception"
    url: capture-exception
    depth: 0
  - value: "Emails"
    url: emails
    depth: 0
  - value: "Flush"
    url: flush
    depth: 0
  - value: "Get feature flag payload"
    url: get-feature-flag-payload
    depth: 0
  - value: "Get feature flag result"
    url: get-feature-flag-result
    depth: 0
  - value: "Evaluate flags"
    url: evaluate-flags
    depth: 0
  - value: "Group identify"
    url: group-identify
    depth: 0
  - value: "Is feature enabled"
    url: is-feature-enabled
    depth: 0
  - value: "Shutdown"
    url: shutdown
    depth: 0
  - value: "Local feature flag evaluator"
    url: local-feature-flag-evaluator
    depth: 0
  - value: "SDK utils"
    url: sdk-utils
    depth: 0
  - value: "EndpointManager"
    url: endpointmanager
    depth: 0
  - value: "FeatureFlagEvaluations"
    url: featureflagevaluations
    depth: 0
  - value: "FlagPoller"
    url: flagpoller
    depth: 0
  - value: "LocalEvaluator"
    url: localevaluator
    depth: 0
  - value: "Types"
    url: types
    depth: 0
---

## Getting started

The Rust SDK runs on your server.

To send your first event, add `posthog` and `tokio`, create a client with your project token from your [project settings](https://app.posthog.com/project/settings), and call `capture`.

A background worker sends queued events, so call `shutdown` before your process exits, or the last events are lost.

### Install

```bash
cargo add posthog@1
cargo add tokio --features rt-multi-thread,macros
```

### Initialize and capture

Create a client with your project API key, capture an event, then shut down so queued events are sent. Without the default `async-client` feature, `client` is synchronous and the calls below drop `.await`.

```rust
use posthog::{client, Event};

#[tokio::main]
async fn main() -> Result<(), posthog::Error> {
    let posthog = client("<ph_project_api_key>").await;

    let mut event = Event::new("user_signed_up", "distinct_id_of_the_user");
    event.insert_prop("plan", "pro")?;
    posthog.capture(event);

    posthog.shutdown().await;
    Ok(())
}
```

`capture` queues the event and returns at once. Call `shutdown` before the process exits.

## Setup

### `posthog::client`

```rust
pub async fn client<C: Into<super::ClientOptions>>(options: C) -> Client
```

Construct an async PostHog client from an API key or [`ClientOptions`].

#### Parameters

- `options`: Either a project API key (for example `"phc_..."`) or a
  configured [`ClientOptions`] value.

#### Returns

A [`Client`] that performs capture and feature flag requests asynchronously.

#### Remarks

This constructor is available with the default `async-client` feature and
must be awaited. Passing a blank API key creates a disabled client.
If the HTTP client cannot be built, logs a warning and creates a disabled
client instead.

[`ClientOptions`]: #clientoptions
[`Client`]: #client

### `posthog::init_global`

```rust
pub async fn init_global<C: Into<ClientOptions>>(options: C) -> Result<(), Error>
```

Initialize the global client singleton.

Use the crate-level `init_global` re-export when you don't need more than
one client instance and don't need to change client options at runtime.

With the `error-tracking` feature (on by default) and
`ErrorTrackingOptions::capture_panics` enabled (opt-in; off by default),
this also installs a process-wide panic hook that captures panics as
`$exception` events through the global client.

#### Parameters

- `options`: Project API key or [`ClientOptions`] used to construct the
  global client.

#### Errors

Returns [`Error::AlreadyInitialized`] if called more than once.

[`ClientOptions`]: #clientoptions
[`Error::AlreadyInitialized`]: #erroralreadyinitialized

## Capture

### `Client::capture`

```rust
pub fn capture(&self, event: Event)
```

Capture the provided event, sending it to PostHog.

#### Parameters

- `event`: Event name, distinct ID, properties, timestamp, groups, and
  optional feature flag state to send.

#### Remarks

Fire-and-forget: the event is handed to the background worker, which
batches, sends, and retries it. Returns once the event is queued — not
once it is delivered, and delivery failures are not surfaced to the
caller. Disabled clients and a full queue drop the event (the latter
with a single warning).

Use [`Client::capture_ai`] for LLM analytics events (`$ai_generation`,
`$ai_span`, …): `capture` never reroutes by event name. The analytics
endpoint still accepts AI events for now, but large ones need `capture_ai`.

[`Client::capture_ai`]: #clientcapture_ai

### `Client::capture_batch`

```rust
pub fn capture_batch(&self, events: Vec<Event>, historical_migration: bool)
```

Capture a collection of events.

#### Parameters

- `events`: Events to send in the batch.
- `historical_migration`: Set to `true` to route events to the
  historical ingestion topic, bypassing the main pipeline.

#### Remarks

Fire-and-forget, like [`Client::capture`]. The batch is enqueued per event
rather than atomically, so if the bounded queue fills partway through, the
remaining events are dropped (with the usual single full-queue warning).

[`Client::capture`]: #clientcapture

### `Client::capture_immediate`

```rust
pub async fn capture_immediate(&self, event: Event) -> Result<CaptureSummary, Error>
```

Capture a single event and await confirmation that the request completed.

The immediate-delivery counterpart to [`Client::capture`]. This is a
convenience wrapper over [`Client::capture_batch_immediate`] with a
one-event batch; see it for full semantics.

[`Client::capture_batch_immediate`]: #clientcapture_batch_immediate
[`Client::capture`]: #clientcapture

### `Client::capture_batch_immediate`

```rust
pub async fn capture_batch_immediate(&self, events: Vec<Event>, historical_migration: bool) -> Result<CaptureSummary, Error>
```

Capture a batch of events and await confirmation that the request
completed, returning a [`CaptureSummary`] describing the outcome.

The immediate-delivery counterpart to [`Client::capture_batch`]. Prefer
the fire-and-forget [`Client::capture`]/[`Client::capture_batch`] for
normal analytics; reach for this only when the caller must know the batch
persisted before advancing its own durable state.

#### Parameters

- `events`: Events to send in a single request.
- `historical_migration`: Route events to the historical ingestion topic.

#### Behavior

Sends inline (bypassing the background worker) and retries transient
failures per the client's retry configuration. A returned `Ok` can still
report unpersisted events — inspect
[`CaptureSummary::all_persisted`]. Does NOT fire `on_error` hooks: the
returned `Result` is the delivery signal. Disabled clients and an empty
(or fully `before_send`-filtered) batch return a default `CaptureSummary`.

#### Errors

Returns [`Error`] when the request is rejected with a terminal status or
the retry budget is exhausted without a successful response.

[`Client::capture`]: #clientcapture
[`CaptureSummary`]: #capturesummary
[`Client::capture_batch`]: #clientcapture_batch
[`CaptureSummary::all_persisted`]: #capturesummaryall_persisted
[`Error`]: #error

### `Event::new`

```rust
pub fn new<S: Into<String>>(event: S, distinct_id: S) -> Self
```

Create a new identified [`Event`]. Unless you have a distinct ID you can
associate with a user, you probably want to use [`Event::new_anon`]
instead.

#### Parameters

- `event`: Event name, such as `"user_signed_up"`.
- `distinct_id`: Stable user or account identifier. For backend events,
  use the same distinct ID your frontend passes to `posthog.identify()`.
  An empty or whitespace-only ID counts as missing: the event becomes
  anonymous, as from [`Event::new_anon`].

[`Event::new_anon`]: #eventnew_anon
[`Event`]: #event

### `Event::new_anon`

```rust
pub fn new_anon<S: Into<String>>(event: S) -> Self
```

Create a new anonymous event.

See <https://posthog.com/docs/data/anonymous-vs-identified-events#how-to-capture-anonymous-events>.

#### Parameters

- `event`: Event name.

#### Remarks

Generates a random distinct ID and sets the `process_person_profile`
option to `false` so PostHog does not create a person profile for the
event. To process one anyway, set the option to `true` with
[`Event::insert_option`]. A `null` or removed option counts as unset,
so the SDK sets `false` again before `before_send`.

[`Event::insert_option`]: #eventinsert_option

### `posthog::capture`

```rust
pub fn capture(event: Event)
```

Capture the provided event using the global client.

#### Remarks

Fire-and-forget, like [`Client::capture`]. No-op if `init_global` has not
run.

[`Client::capture`]: #clientcapture

## Alias

### `Client::alias`

```rust
pub fn alias<P: Into<String>, D: Into<String>>(&self, previous_id: P, distinct_id: D)
```

Merge two distinct IDs onto the same person by sending a `$create_alias`
event.

See <https://posthog.com/docs/product-analytics/identify#alias-assigning-multiple-distinct-ids-to-the-same-user>.

#### Parameters

- `previous_id`: ID already known to PostHog, such as an anonymous ID.
- `distinct_id`: ID it should be merged into, such as a logged-in user ID.

#### Remarks

Fire-and-forget, like [`Client::capture`]. A blank ID on either side
cannot describe a merge, so the event is dropped with a warning rather
than sent.

#### Examples

```rust
let client = posthog::client("phc_project_api_key").await;

// The visitor browsed anonymously, then logged in.
client.alias("anon-abc123", "user-42");
```

[`Client::capture`]: #clientcapture

## Capture AI

### `Client::capture_ai`

```rust
pub fn capture_ai(&self, event: Event)
```

Capture an AI (LLM analytics) event, sending it to PostHog's AI
ingestion endpoint.

#### Parameters

- `event`: The AI event (`$ai_generation`, `$ai_span`, `$ai_trace`,
  `$ai_embedding`, …) with its `$ai_*` properties.

#### Remarks

Fire-and-forget, like [`Client::capture`], on a separate lane: its own
background worker, queue (`capture_ai_max_queue_size`), retry state, and
endpoint, so a multi-MB AI event can never delay or displace analytics
events. The lane batches by size (about 5 MiB per request), compresses
bodies per `capture_ai_compression` (unset sends them raw; zstd is
recommended), and drops an event locally — with a warning naming only
the event and its byte size — when its serialized properties exceed
8 MiB, the backend's per-event ceiling. The worker starts on the first
call.

The SDK does not inspect the event name. The backend accepts only its
AI event names on this endpoint and reports anything else as a per-event
`drop`, which reaches an `on_error` hook (see
[`CaptureFailure::endpoint`](crate::CaptureFailure::endpoint)) or, with no
hook, one aggregate warning per batch.

[`Client::capture`]: #clientcapture
[crate::CaptureFailure::endpoint]: #cratecapturefailureendpoint

### `Client::capture_ai_batch`

```rust
pub fn capture_ai_batch(&self, events: Vec<Event>, historical_migration: bool)
```

Capture a batch of AI events on the AI lane.

#### Parameters

- `events`: AI events to send.
- `historical_migration`: Kept for parity with [`Client::capture_batch`]
  and passed through to the backend, which currently ignores it for AI
  events.

#### Remarks

Fire-and-forget, like [`Client::capture_ai`]. Events are enqueued per
event, so if the bounded queue fills partway through, the remaining
events are dropped (with the usual single full-queue warning). The lane
splits the batch into requests of about 5 MiB each.

[`Client::capture_ai`]: #clientcapture_ai
[`Client::capture_batch`]: #clientcapture_batch

### `Client::capture_ai_immediate`

```rust
pub async fn capture_ai_immediate(&self, event: Event) -> Result<CaptureSummary, Error>
```

Capture a single AI event and await confirmation that the request
completed.

The immediate-delivery counterpart to [`Client::capture_ai`]. This is a
convenience wrapper over [`Client::capture_ai_batch_immediate`] with a
one-event batch; see it for full semantics.

[`Client::capture_ai`]: #clientcapture_ai
[`Client::capture_ai_batch_immediate`]: #clientcapture_ai_batch_immediate

### `Client::capture_ai_batch_immediate`

```rust
pub async fn capture_ai_batch_immediate(&self, events: Vec<Event>, historical_migration: bool) -> Result<CaptureSummary, Error>
```

Capture a batch of AI events and await confirmation that every request
completed, returning a [`CaptureSummary`] describing the outcome.

The immediate-delivery counterpart to [`Client::capture_ai_batch`].

#### Parameters

- `events`: AI events to send.
- `historical_migration`: Kept for parity with
  [`Client::capture_batch_immediate`] and passed through to the backend,
  which currently ignores it for AI events.

#### Behavior

Like [`Client::capture_batch_immediate`], but on the AI endpoint with
`capture_ai_compression`, and the batch is split into requests of about 5 MiB
each, sent in order. Each request retries transient failures and
per-event `retry` verdicts per the client's retry configuration; the
summary merges every request's verdicts. An event whose serialized
`properties` are over 8 MiB is not sent, as the backend would refuse it.
It gets a local `drop` verdict with `details` `ai_event_too_big` in the
summary, and counts in [`CaptureSummary::submitted`] and
[`CaptureSummary::not_persisted`].

#### Errors

Returns [`Error`] as soon as one request is rejected with a terminal
status or exhausts its retry budget; requests sent before it may already
have persisted. An `Err` therefore means "not confirmed", not "nothing
was stored".

[`Client::capture_batch_immediate`]: #clientcapture_batch_immediate
[`CaptureSummary::submitted`]: #capturesummarysubmitted
[`CaptureSummary`]: #capturesummary
[`Error`]: #error
[`CaptureSummary::not_persisted`]: #capturesummarynot_persisted
[`Client::capture_ai_batch`]: #clientcapture_ai_batch

### `posthog::capture_ai`

```rust
pub fn capture_ai(event: Event)
```

Capture the provided AI event using the global client.

#### Remarks

Fire-and-forget, like [`Client::capture_ai`]. No-op if `init_global` has
not run.

[`Client::capture_ai`]: #clientcapture_ai

## Capture exception

### `Client::capture_exception`

```rust
pub async fn capture_exception<E>(&self, error: &E) -> Result<(), Error>
where
    E: StdError + ?Sized,
```

Capture a Rust error personlessly, sending it to PostHog Error Tracking.

The error's type, message, and full `source()` chain are sent as
`$exception_list`, with a stacktrace of the capture site attached to
the first entry (see `ErrorTrackingOptions::capture_stacktrace`).

Accepts any [`std::error::Error`], including `&dyn Error`. A
`Box<dyn Error>` does not implement `Error` itself, so pass the
dereferenced trait object: `capture_exception(&*boxed)`.

To associate the exception with a person or attach custom properties,
groups, a fingerprint, or a severity level, use
[`Client::capture_exception_with`].

#### Examples

```rust
let client = posthog::client("phc_project_api_key").await;
let error = std::io::Error::other("checkout failed");

client.capture_exception(&error).await?;
```

[`std::error::Error`]: #stderrorerror
[`Client::capture_exception_with`]: #clientcapture_exception_with

### `Client::capture_exception_with`

```rust
pub async fn capture_exception_with<E>(&self, error: &E, options: CaptureExceptionOptions) -> Result<(), Error>
where
    E: StdError + ?Sized,
```

Capture a Rust error with optional context, sending it to PostHog
Error Tracking.

Set [`CaptureExceptionOptions::distinct_id`] to associate the exception
with a person; without it the exception is captured personlessly.

#### Examples

```rust
use posthog::CaptureExceptionOptions;

let client = posthog::client("phc_project_api_key").await;
let error = std::io::Error::other("checkout failed");

client
    .capture_exception_with(
        &error,
        CaptureExceptionOptions::new()
            .distinct_id("user-123")
            .property("route", "/checkout")?,
    )
    .await?;
```

[`CaptureExceptionOptions::distinct_id`]: #captureexceptionoptionsdistinct_id

### `posthog::capture_exception`

```rust
pub async fn capture_exception<E>(error: &E) -> Result<(), Error>
where
    E: StdError + ?Sized,
```

Capture a Rust error personlessly using the global client.

### `posthog::capture_exception_with`

```rust
pub async fn capture_exception_with<E>(error: &E, options: error_tracking::CaptureExceptionOptions) -> Result<(), Error>
where
    E: StdError + ?Sized,
```

Capture a Rust error with optional context using the global client.

## Emails

### `Client::emails`

```rust
pub fn emails(&self) -> Emails<'_>
```

Transactional email: send email from templates made in PostHog or from
content your code rendered, and read the status of each email sent.

Needs `secret_key` set to a Project Secret API Key with the
`transactional_email:write` scope. See [`Emails`] for setup.

[`Emails`]: #emails

## Flush

### `Client::flush`

```rust
pub async fn flush(&self)
```

Flush queued events, returning once the worker has attempted delivery of
everything queued before this call. Transient failures are kept for retry
(the call still returns without error). A no-op for disabled clients.

### `posthog::flush`

```rust
pub async fn flush()
```

Flush the global client's queued events, awaiting the worker's next delivery
attempt. No-op if `init_global` has not run.

`capture` only enqueues, and the global client lives in a `static` whose
`Drop` never runs at process exit, so call this (or [`shutdown`]) before
exiting to avoid losing buffered events.

[`shutdown`]: #shutdown

## Get feature flag payload

### `FeatureFlagEvaluations::get_flag_payload`

```rust
pub fn get_flag_payload(&self, key: &str) -> Option<Value>
```

Return the JSON payload associated with `key`, if any.

#### Remarks

This call does **not** count as an access and does **not** fire any
event, matching the behavior documented for server-side SDKs.

## Get feature flag result

### `FeatureFlagEvaluations::get_flag`

```rust
pub fn get_flag(&self, key: &str) -> Option<FlagValue>
```

Look up the value of `key`.

#### Returns

- `None` when the flag is not in the snapshot,
- `Some(FlagValue::Boolean(false))` when disabled,
- `Some(FlagValue::String(variant))` for a multivariate match,
- `Some(FlagValue::Boolean(true))` when enabled with no variant.

Records the access and fires (deduplicated) `$feature_flag_called`.

## Evaluate flags

### `Client::evaluate_flags_with_context`

```rust
pub async fn evaluate_flags_with_context(&self, context: &RequestContext, options: EvaluateFlagsOptions) -> Result<FeatureFlagEvaluations, Error>
```

Uses this context's identity and pins defaults for later flag access events.

### `Client::evaluate_flags`

```rust
pub async fn evaluate_flags<S: Into<String>>(&self, distinct_id: S, options: EvaluateFlagsOptions) -> Result<FeatureFlagEvaluations, Error>
```

Evaluate feature flags for `distinct_id`, returning a
[`FeatureFlagEvaluations`] snapshot.

Each `is_enabled` / `get_flag` call on the returned snapshot fires a
dedup-aware `$feature_flag_called` event with full metadata, and the
snapshot can be passed to [`Event::with_flags`] so a downstream
[`Client::capture`] inherits `$feature/<key>` and `$active_feature_flags`
without an extra `/flags` request.

#### Parameters

- `distinct_id`: User distinct ID. Empty values return an empty snapshot.
- `options`: Optional groups, properties, GeoIP override, local-only
  mode, and flag-key filtering.

#### Errors

Returns [`Error::Connection`] or [`Error::Serialization`] when remote
evaluation is required and the `/flags` request fails before any local
results are available.


[crate::Event::with_flags]: #crateeventwith_flags
[`Client::capture`]: #clientcapture
[`Error::Connection`]: #errorconnection
[`FeatureFlagEvaluations`]: #featureflagevaluations
[`Error::Serialization`]: #errorserialization

## Group identify

### `Client::group_identify`

```rust
pub fn group_identify<T: Into<String>, K: Into<String>, P: Serialize>(&self, group_type: T, group_key: K, properties: P) -> Result<(), Error>
```

Create or update a group and set its properties by sending a `$groupidentify`
event.

See <https://posthog.com/docs/product-analytics/group-analytics#setting-group-properties>.

#### Parameters

- `group_type`: Group type, such as `"company"`, `"project"`, or `"organization"`.
- `group_key`: Unique identifier for the group, such as an ID in your database.
- `properties`: Any serializable object or JSON map representing group properties.

#### Remarks

Fire-and-forget, like [`Client::capture`], for a blank `group_type` or
`group_key`: the event is dropped with a warning rather than sent.

The event's distinct id is the request context's distinct id when one is
set, else `$<group_type>_<group_key>`. The context session id is never added.

#### Errors

Returns [`Error::Serialization`] if `properties` fails to serialize to
JSON, or if it does not serialize to a JSON object (PostHog requires
`$group_set` to be an object).

#### Examples

```rust
use serde_json::json;

let client = posthog::client("phc_project_api_key").await;

client.group_identify(
    "company",
    "company_id_in_your_db",
    json!({
        "name": "Awesome Inc.",
        "employees": 11,
    }),
)?;
```

[`Client::capture`]: #clientcapture
[`Error::Serialization`]: #errorserialization

## Is feature enabled

### `FeatureFlagEvaluations::is_enabled`

```rust
pub fn is_enabled(&self, key: &str) -> bool
```

Whether `key` is enabled. Records the access and fires (deduplicated)
`$feature_flag_called`.

#### Returns

`true` for enabled boolean flags or matched multivariate variants, and
`false` for disabled or missing flags.

## Shutdown

### `Client::shutdown`

```rust
pub async fn shutdown(&self)
```

Flush, stop the background worker, and join it. Idempotent: subsequent
calls are no-ops. After shutdown, `capture` drops events and
[`emails`](Self::emails) refuses requests with `client_closed`. Email
requests already in flight, retries included, get until
`shutdown_timeout_ms` from the start of shutdown to finish. When called
from a transport callback, queues shutdown without waiting for email
requests or joining the current worker thread.

[Self::emails]: #selfemails

### `posthog::shutdown`

```rust
pub async fn shutdown()
```

Flush and stop the global client's background worker. Idempotent; no-op if
`init_global` has not run.

## Local feature flag evaluator

### `Client::evaluate_feature_flag_locally`

```rust
pub fn evaluate_feature_flag_locally(&self, flag: &FeatureFlag, distinct_id: &str, person_properties: &HashMap<String, serde_json::Value>, groups: &HashMap<String, String>, group_properties: &HashMap<String, HashMap<String, serde_json::Value>>) -> Result<FlagValue, Error>
```

Evaluate a supplied feature flag definition locally.

`groups` and `group_properties` are only consulted when the flag (or one
of its conditions) targets a group; pass empty maps for person flags.

#### Parameters

- `flag`: Feature flag definition to evaluate.
- `distinct_id`: User distinct ID.
- `person_properties`: Person properties available to release
  conditions.
- `groups`: Group keys for group-targeted flags.
- `group_properties`: Group properties for group-targeted release
  conditions.

#### Errors

Returns [`Error::InconclusiveMatch`] when the flag cannot be evaluated
locally with the supplied context.

[`Error::InconclusiveMatch`]: #errorinconclusivematch

## SDK utils

### `posthog::DEFAULT_HOST`

```rust
pub const DEFAULT_HOST: &str = US_INGESTION_ENDPOINT
```

Default host (US by default)

### `posthog::EU_INGESTION_ENDPOINT`

```rust
pub const EU_INGESTION_ENDPOINT: &str = "https://eu.i.posthog.com"
```

EU ingestion endpoint

### `posthog::US_INGESTION_ENDPOINT`

```rust
pub const US_INGESTION_ENDPOINT: &str = "https://us.i.posthog.com"
```

US ingestion endpoint

### `posthog::match_feature_flag`

```rust
pub fn match_feature_flag(flag: &FeatureFlag, distinct_id: &str, person_properties: &std::collections::HashMap<String, serde_json::Value>, groups: &std::collections::HashMap<String, String>, group_properties: &std::collections::HashMap<String, std::collections::HashMap<String, serde_json::Value>>, group_type_mapping: &std::collections::HashMap<String, String>) -> Result<FlagValue, InconclusiveMatchError>
```

Evaluate a feature flag definition against person and optional group
context.

#### Parameters

- `flag`: Feature flag definition to evaluate.
- `distinct_id`: Distinct ID used for person bucketing.
- `person_properties`: Person properties available to release conditions.
- `groups`: Group keys for group-targeted flags, keyed by group type.
- `group_properties`: Group properties for group-targeted flags.
- `group_type_mapping`: Mapping from PostHog group type index to group type
  name.

#### Returns

The matched flag value: `Boolean(false)` for inactive or unmatched flags,
`Boolean(true)` for matched boolean flags, or `String(variant)` for matched
multivariate flags.

#### Errors

Returns [`InconclusiveMatchError`] when required properties are missing or an
operator cannot be evaluated locally.

[`InconclusiveMatchError`]: #inconclusivematcherror

### `posthog::match_feature_flag_with_context`

```rust
pub fn match_feature_flag_with_context(flag: &FeatureFlag, person_properties: &std::collections::HashMap<String, serde_json::Value>, ctx: &EvaluationContext<'_>) -> Result<FlagValue, InconclusiveMatchError>
```

Match a feature flag with full context (cohorts, other flags).

This version supports cohort membership checks and flag dependency checks.
`person_properties` carries person-level property values; group-level
properties are looked up from `ctx.group_properties` when a condition (or
the flag itself) targets a group via `aggregation_group_type_index`.

#### Errors

Returns [`InconclusiveMatchError`] when local evaluation cannot determine a
result with the provided context.

[`InconclusiveMatchError`]: #inconclusivematcherror

### `posthog::match_property_with_context`

```rust
pub fn match_property_with_context(property: &Property, properties: &std::collections::HashMap<String, serde_json::Value>, ctx: &EvaluationContext<'_>) -> Result<bool, InconclusiveMatchError>
```

Match a property with additional context for cohorts and flag dependencies.

Use this when evaluating local feature flag conditions that can reference
cohort membership (`type = "cohort"`) or another feature flag via
`$feature/<flag-key>`.

#### Errors

Returns [`InconclusiveMatchError`] when the property, cohort, or dependent
flag cannot be evaluated from the supplied context.

[`InconclusiveMatchError`]: #inconclusivematcherror

### `posthog::disable_global`

```rust
pub fn disable_global()
```

Prevent the global client from being initialized.

#### Remarks

This does *not* prevent use of a global client that was already initialized.

### `posthog::global_is_disabled`

```rust
pub fn global_is_disabled() -> bool
```

Return `true` if global client initialization has been disabled.

#### Remarks

A disabled global client can still be used if it was initialized before it
was disabled.

## EndpointManager

### `EndpointManager`

```rust
pub struct EndpointManager
```

Manages PostHog API endpoints and host configuration.

This low-level helper normalizes app hosts such as
`https://us.posthog.com` to ingestion hosts such as
[`US_INGESTION_ENDPOINT`].

[`US_INGESTION_ENDPOINT`]: #us_ingestion_endpoint

### `EndpointManager::new`

```rust
pub fn new(host: String) -> Self
```

Create a new endpoint manager for `host`.

`host` may be an app host (for example `https://eu.posthog.com`), an
ingestion host, or a custom reverse-proxy host.

### `EndpointManager::determine_server_host`

```rust
pub fn determine_server_host(host: &str) -> String
```

Determine the ingestion host used for API calls.

Maps PostHog app hosts to their ingestion equivalents and removes a
trailing slash. Custom hosts are returned unchanged except for the
trailing slash.

### `EndpointManager::base_host`

```rust
pub fn base_host(&self) -> &str
```

Get the normalized base host URL used for API calls.

### `EndpointManager::raw_host`

```rust
pub fn raw_host(&self) -> &str
```

Get the raw host as provided by the user.

### `EndpointManager::build_url`

```rust
pub fn build_url(&self, endpoint: Endpoint) -> String
```

Build a full URL for a given SDK endpoint.

### `EndpointManager::build_custom_url`

```rust
pub fn build_custom_url(&self, path: &str) -> String
```

Build a URL with a custom path relative to the base host.

The path may be passed with or without a leading `/`.

### `EndpointManager::build_local_eval_url`

```rust
pub fn build_local_eval_url(&self, token: &str) -> String
```

Build the local evaluation definitions URL with a project token.

The token is included as a query parameter, so avoid logging this URL in
production diagnostics.

### `EndpointManager::api_host`

```rust
pub fn api_host(&self) -> String
```

Get the base host for API operations without a trailing slash or path.

## FeatureFlagEvaluations

### `FeatureFlagEvaluations`

```rust
pub struct FeatureFlagEvaluations
```

A snapshot of evaluated feature flags for one `distinct_id`.

Returned by [`Client::evaluate_flags`](crate::Client::evaluate_flags). Reading
flags via [`is_enabled`] or [`get_flag`] both records the access (so it can be
later attached to a capture event) and emits a deduplicated
`$feature_flag_called` event. [`get_flag_payload`] is intentionally event-free.


[FeatureFlagEvaluations::get_flag_payload]: #featureflagevaluationsget_flag_payload
[FeatureFlagEvaluations::get_flag]: #featureflagevaluationsget_flag
[crate::Client::evaluate_flags]: #crateclientevaluate_flags
[FeatureFlagEvaluations::is_enabled]: #featureflagevaluationsis_enabled

### `FeatureFlagEvaluations::keys`

```rust
pub fn keys(&self) -> Vec<String>
```

All flag keys present in this snapshot.

### `FeatureFlagEvaluations::only_accessed`

```rust
pub fn only_accessed(&self) -> Self
```

A clone of the snapshot containing only flags whose values were read via
[`is_enabled`](Self::is_enabled) or [`get_flag`](Self::get_flag) before
this call.

Order-dependent: if nothing has been accessed yet, the returned snapshot
is empty. Pre-access the flags you want to attach before calling this.

[Self::get_flag]: #selfget_flag
[Self::is_enabled]: #selfis_enabled

### `FeatureFlagEvaluations::only`

```rust
pub fn only(&self, keys: &[&str]) -> Self
```

A clone of the snapshot containing only the listed `keys` (preserving
records). Unknown keys are dropped and surfaced via a single warning.

Use this before [`Event::with_flags`](crate::Event::with_flags) to limit
the `$feature/<key>` properties attached to a captured event.

[crate::Event::with_flags]: #crateeventwith_flags

## FlagPoller

### `FlagPoller`

```rust
pub struct FlagPoller
```

Synchronous poller for feature flag definitions.

Runs a background thread that periodically fetches flag definitions from
the PostHog API and updates the shared cache. Use this for blocking/sync
applications. With the `async-client` feature enabled, use
[`AsyncFlagPoller`] for async applications instead.

[`AsyncFlagPoller`]: #asyncflagpoller

### `FlagPoller::new`

```rust
pub fn new(config: LocalEvaluationConfig, cache: FlagCache) -> Self
```

Create a synchronous flag definition poller.

If the HTTP client cannot be built, logs a warning and leaves polling
disabled. [`Self::load_flags`] then returns [`Error::Connection`].

#### Parameters

- `config`: Credentials, host, polling interval, and request timeout.
- `cache`: Shared cache updated by the poller.

[`Self::load_flags`]: #selfload_flags
[`Error::Connection`]: #errorconnection

### `FlagPoller::start`

```rust
pub fn start(&mut self)
```

Start the polling thread.

Performs an initial synchronous load, then refreshes definitions in the
background until [`FlagPoller::stop`] is called or the poller is dropped.

[`FlagPoller::stop`]: #flagpollerstop

### `FlagPoller::load_flags`

```rust
pub fn load_flags(&self) -> Result<(), Error>
```

Load flags synchronously and update the cache once.

#### Errors

Returns [`Error::Connection`] for request failures or non-success HTTP
statuses, and [`Error::Serialization`] when the response cannot be
parsed.

[`Error::Serialization`]: #errorserialization
[`Error::Connection`]: #errorconnection

### `FlagPoller::stop`

```rust
pub fn stop(&mut self)
```

Stop the polling thread and wait for it to exit.

## LocalEvaluator

### `LocalEvaluator`

```rust
pub struct LocalEvaluator
```

Evaluates feature flags using locally cached definitions.

The evaluator reads from a [`FlagCache`] to determine flag values without
making network requests. Supports cohort membership checks and flag
dependencies through the evaluation context.

[`FlagCache`]: #flagcache

### `LocalEvaluator::new`

```rust
pub fn new(cache: FlagCache) -> Self
```

Create an evaluator backed by a shared [`FlagCache`].

[`FlagCache`]: #flagcache

### `LocalEvaluator::cache`

```rust
pub fn cache(&self) -> &FlagCache
```

Access the underlying flag cache (e.g. to read group type mappings).

### `LocalEvaluator::evaluate_flag`

```rust
pub fn evaluate_flag(&self, key: &str, distinct_id: &str, person_properties: &HashMap<String, serde_json::Value>, groups: &HashMap<String, String>, group_properties: &HashMap<String, HashMap<String, serde_json::Value>>) -> Result<Option<FlagValue>, InconclusiveMatchError>
```

Evaluate a feature flag locally with full context support.

Supports cohort membership checks, flag dependency evaluation, and
group / mixed-targeting flags. `groups` and `group_properties` are
only consulted when the flag (or one of its conditions) targets a
group via `aggregation_group_type_index`; pass empty maps for
person-targeted flags.

#### Returns

`Ok(Some(value))` when the flag is present and evaluated,
`Ok(None)` when the flag is absent from the cache.

#### Errors

Returns [`InconclusiveMatchError`] when required properties, cohorts, or
dependent flags are unavailable locally.

[`InconclusiveMatchError`]: #inconclusivematcherror

### `LocalEvaluator::evaluate_flag_simple`

```rust
pub fn evaluate_flag_simple(&self, key: &str, distinct_id: &str, person_properties: &HashMap<String, serde_json::Value>, groups: &HashMap<String, String>, group_properties: &HashMap<String, HashMap<String, serde_json::Value>>) -> Result<Option<FlagValue>, InconclusiveMatchError>
```

Evaluate a feature flag locally without cohort or flag dependency
support.

Use this when you know the flag doesn't have cohort or flag dependency
conditions.

#### Returns

`Ok(Some(value))` when the flag is present and evaluated,
`Ok(None)` when the flag is absent from the cache.

#### Errors

Returns [`InconclusiveMatchError`] when required properties are
unavailable locally.

[`InconclusiveMatchError`]: #inconclusivematcherror

### `LocalEvaluator::evaluate_all_flags`

```rust
pub fn evaluate_all_flags(&self, distinct_id: &str, person_properties: &HashMap<String, serde_json::Value>, groups: &HashMap<String, String>, group_properties: &HashMap<String, HashMap<String, serde_json::Value>>) -> HashMap<String, Result<FlagValue, InconclusiveMatchError>>
```

Get all flags and evaluate them with full context support.

The returned map is keyed by feature flag key. Each value can be an
inconclusive error if that particular flag could not be evaluated from
the supplied context.

## Types

### `RequestContext`

```rust
pub struct RequestContext
```

Analytics defaults for one request. Header identity is not proof of authentication.

#### `RequestContext::new`

```rust
pub fn new() -> Self
```

#### `RequestContext::distinct_id`

```rust
pub fn distinct_id(self, distinct_id: impl Into<String>) -> Self
```

#### `RequestContext::session_id`

```rust
pub fn session_id(self, session_id: impl Into<String>) -> Self
```

#### `RequestContext::property`

```rust
pub fn property(self, key: impl Into<String>, value: impl Serialize) -> Result<Self, Error>
```

#### `RequestContext::option`

```rust
pub fn option(self, key: impl Into<String>, value: impl Serialize) -> Result<Self, Error>
```

Sets a capture option on every event in this context. An option the event sets wins,
and a `null` value sets nothing.

#### `RequestContext::get_distinct_id`

```rust
pub fn get_distinct_id(&self) -> Option<&str>
```

#### `RequestContext::get_session_id`

```rust
pub fn get_session_id(&self) -> Option<&str>
```

#### `RequestContext::current`

```rust
pub fn current() -> Option<Self>
```

Returns a snapshot. Spawned tasks must receive and scope an explicit clone.

#### `RequestContext::scope`

```rust
pub async fn scope<F: std::future::Future>(self, future: F) -> <F as >::Output
```

Context exists only while the future is polled, including error and cancellation paths.

#### `RequestContext::event`

```rust
pub fn event(&self, name: impl Into<String>) -> Event
```

Without identity, this uses the SDK's ordinary personless event behavior.

#### `RequestContext::apply_to_event`

```rust
pub fn apply_to_event(&self, event: &mut Event)
```

Pins these defaults before delivery. Explicit event identity, properties and options win.

#### `RequestContext::exception_options`

```rust
pub fn exception_options(&self, options: CaptureExceptionOptions) -> CaptureExceptionOptions
```

### `BeforeSendHook`

```rust
pub struct BeforeSendHook
```

Hook that can modify or discard events before they are sent.

Hooks run before serialization. Return `Some(event)` to continue sending the
event, or `None` to drop it.

Hook panics are caught and cause the current event to be dropped. If a hook
keeps mutable state, a panic can leave that state partially updated; the SDK
recovers the hook mutex and subsequent events continue through the same hook.

#### `BeforeSendHook::new`

```rust
pub fn new<F>(hook: F) -> Self
where
    F: FnMut(Event) -> Option<Event> + Send + 'static,
```

Create a new before-send hook.

### `CaptureCompression`

```rust
pub enum CaptureCompression
```

**Variants**

- `Gzip`
- `Deflate`
- `Br`
- `Zstd`

Request-body compression algorithm for the capture pipelines.

When set on [`ClientOptions`], capture requests are compressed and the
matching `Content-Encoding` header is sent. The variant string matches the
HTTP `Content-Encoding` token the server expects. All four variants are
accepted by the capture endpoint.

[`ClientOptions`]: #clientoptions

### `CaptureSummary`

```rust
pub struct CaptureSummary
```

The outcome of an immediate capture ([`Client::capture_immediate`] /
[`Client::capture_batch_immediate`]), returned once the SDK has a terminal
result for the batch — the request succeeded, or the retry budget was spent
(which is an [`Err`] instead).

A returned `CaptureSummary` means the capture request itself succeeded (HTTP
`2xx`). The backend reports a per-event verdict, so a `2xx` can still leave
some events unpersisted (`drop`/`retry`) — check
[`all_persisted`](Self::all_persisted) / [`not_persisted`](Self::not_persisted)
before treating the batch as fully durable.

`#[non_exhaustive]`: fields are read through accessors so more can be added
without breaking callers.


[crate::Client::capture_batch_immediate]: #crateclientcapture_batch_immediate
[Self::not_persisted]: #selfnot_persisted
[crate::Client::capture_immediate]: #crateclientcapture_immediate
[`Err`]: #err
[Self::all_persisted]: #selfall_persisted

#### `CaptureSummary::submitted`

```rust
pub fn submitted(&self) -> usize
```

Number of events that reached a verdict (after `before_send` filtering):
the events sent on the wire, plus any AI event the SDK did not send
because it was over the size limit. Such an event has a local `drop`
verdict in [`event_results`](Self::event_results).

[Self::event_results]: #selfevent_results

#### `CaptureSummary::not_persisted`

```rust
pub fn not_persisted(&self) -> usize
```

Number of submitted events the backend did not persist.

`submitted` minus the events with an `ok`/`warning` verdict, so it counts
both `drop`/`retry` verdicts and any submitted event the backend omitted
from its response, including copies dropped as duplicates.

#### `CaptureSummary::all_persisted`

```rust
pub fn all_persisted(&self) -> bool
```

Whether every submitted event was persisted (`not_persisted() == 0`).

Note this is **vacuously true when nothing was sent** (`submitted() == 0`),
which is what a disabled client or a fully `before_send`-filtered batch
returns. Callers that advance durable state on the strength of an immediate
delivery (e.g. committing an upstream offset) must therefore also check
`submitted()` against the number of events they intended to send — do not
gate durability on `all_persisted()` alone.

#### `CaptureSummary::event_results`

```rust
pub fn event_results(&self) -> &HashMap<Uuid, EventResult>
```

Per-event server verdicts. Includes persisted
(`ok`/`warning`) and unpersisted (`drop`/`retry`) verdicts; filter by
[`EventStatus`](crate::EventStatus) to isolate failures. May omit events
the backend did not report on — see [`not_persisted`](Self::not_persisted).

An entry's [`duplicates_dropped`](EventResult::duplicates_dropped) counts
lost copies that shared its uuid; they have no entry of their own.

[crate::EventStatus]: #crateeventstatus
[EventResult::duplicates_dropped]: #eventresultduplicates_dropped
[Self::not_persisted]: #selfnot_persisted

### `ClientOptions`

```rust
pub struct ClientOptions
```

Configuration options for the PostHog client.

Use [`ClientOptionsBuilder`] to construct options with custom settings, or
create options directly from a project API key with
`ClientOptions::from("your-api-key")`.

#### Example

```rust
use posthog::ClientOptionsBuilder;

let options = ClientOptionsBuilder::default()
    .api_key("your-project-api-key".to_string())
    .host("https://eu.posthog.com")
    .build()
    .unwrap();
```

[`ClientOptionsBuilder`]: #clientoptionsbuilder

#### `ClientOptions::is_disabled`

```rust
pub fn is_disabled(&self) -> bool
```

Check whether the client is disabled.

A client is disabled when configured with `disabled(true)`, when the
project API key is missing or blank after trimming, or when it is a
personal API key (`phx_...`), which PostHog never accepts for capture.

### `ClientOptionsBuilder`

```rust
pub struct ClientOptionsBuilder
```

Builder for [`ClientOptions`](struct.ClientOptions.html).

#### `ClientOptionsBuilder::host`

```rust
pub fn host<VALUE: ::derive_builder::export::core::convert::Into<String>>(&mut self, value: VALUE) -> &mut Self
```

Host URL for the PostHog API. Defaults to the US ingestion endpoint.
App hosts such as `https://eu.posthog.com` are normalized to ingestion
hosts before requests are sent.

#### `ClientOptionsBuilder::api_key`

```rust
pub fn api_key(&mut self, value: String) -> &mut Self
```

PostHog project API key (project token). If missing or blank, the client
is disabled.

#### `ClientOptionsBuilder::http_client`

```rust
pub fn http_client(&mut self, value: reqwest::Client) -> &mut Self
```

Optional async HTTP client for immediate capture, flags, and local evaluation.
Defaults to an SDK-created client. Clones share the supplied connection pool.
The SDK still applies `request_timeout_seconds` to capture and local
evaluation, and `feature_flags_request_timeout_seconds` to remote flags.
These options replace the supplied client's request timeout, even when the
client's timeout is shorter. Background capture uses `blocking_http_client`;
see its documentation for the limits of `shutdown_timeout_ms`.

Only supply clients whose default headers and cookies are safe to send to
PostHog. Do not share clients carrying credentials for unrelated services;
the SDK cannot inspect or filter those defaults. The SDK keeps the
supplied client's redirect policy; the capture and `/flags` clients it
builds follow only 307 and 308 redirects to the same scheme, host and port.

#### `ClientOptionsBuilder::blocking_http_client`

```rust
pub fn blocking_http_client(&mut self, value: reqwest::blocking::Client) -> &mut Self
```

Optional blocking HTTP client for blocking requests and background capture.
Defaults to SDK-created clients. Clones share the supplied connection pool.
The SDK still applies `request_timeout_seconds` to capture and local
evaluation, and `feature_flags_request_timeout_seconds` to remote flags.
These options replace the supplied client's request timeout, even when the
client's timeout is shorter.

`shutdown_timeout_ms` bounds shutdown draining and caps requests started
during that drain at the remaining deadline. It does not interrupt work
already underway. An in-flight request can delay `shutdown()` or `Drop`
until `request_timeout_seconds` expires; an automatic multi-batch flush
already underway can delay teardown by several request timeouts.
It is not a hard limit on the total time spent waiting for the worker.

Only supply clients whose default headers and cookies are safe to send to
PostHog. Do not share clients carrying credentials for unrelated services;
the SDK cannot inspect or filter those defaults. The SDK keeps the
supplied client's redirect policy; the capture and `/flags` clients it
builds follow only 307 and 308 redirects to the same scheme, host and port.

Create and finally drop application-owned blocking clients, including handles
held in options and builders, outside async runtimes. SDK shutdown does not
invalidate application-held clones. Both setters require a compatible reqwest
version (currently 0.13).

#### `ClientOptionsBuilder::request_timeout_seconds`

```rust
pub fn request_timeout_seconds(&mut self, value: u64) -> &mut Self
```

Per-request timeout in seconds for capture, batch, and local evaluation
definition requests. Defaults to `30`.
Also applies to custom HTTP clients, replacing their request timeout even
when that timeout is shorter. Remote flags use `feature_flags_request_timeout_seconds`.

#### `ClientOptionsBuilder::secret_key`

```rust
pub fn secret_key<VALUE: ::derive_builder::export::core::convert::Into<String>>(&mut self, value: VALUE) -> &mut Self
```

Secret key used for local feature flag evaluation and remote config.

Accepts either a Personal API Key (`phx_...`) or a Project Secret API
Key (`phs_...`). Required when `enable_local_evaluation` is `true`.

[`Client::emails`] needs a Project Secret API Key with the
`transactional_email:write` scope to send email, or
`transactional_email:read` to read an email's status.

[`Client::emails`]: #clientemails

#### `ClientOptionsBuilder::app_host`

```rust
pub fn app_host<VALUE: ::derive_builder::export::core::convert::Into<String>>(&mut self, value: VALUE) -> &mut Self
```

Your PostHog app host, such as `https://posthog.example.com`, for
[`Client::emails`]. Needed only when `host` is not PostHog Cloud, such
as a reverse proxy: email requests carry the secret key, so they never
go to a custom `host`. Without it, a PostHog Cloud `host` such as
`https://eu.i.posthog.com` sends email to its region's app host.

[`Client::emails`]: #clientemails

#### `ClientOptionsBuilder::email_request_timeout_seconds`

```rust
pub fn email_request_timeout_seconds(&mut self, value: u64) -> &mut Self
```

Per-request timeout in seconds for [`Client::emails`]. Defaults to `15`.
Also applies to custom HTTP clients, replacing their request timeout.

[`Client::emails`]: #clientemails

#### `ClientOptionsBuilder::email_request_max_retries`

```rust
pub fn email_request_max_retries(&mut self, value: u32) -> &mut Self
```

Maximum number of retries of an email request after a network error,
a rate limit, a send with the same idempotency key in flight, or an
unavailable service. Defaults to `2`. Set to `0` when a job runner
already retries the send. A send can override it.

#### `ClientOptionsBuilder::migrated_from`

```rust
pub fn migrated_from<VALUE: ::derive_builder::export::core::convert::Into<String>>(&mut self, value: VALUE) -> &mut Self
```

The email provider your sends moved from, such as `"resend"`, sent with
each email request so PostHog knows where your sends came from.

#### `ClientOptionsBuilder::enable_local_evaluation`

```rust
pub fn enable_local_evaluation(&mut self, value: bool) -> &mut Self
```

Enable local evaluation of feature flags using a background definitions
poller.

#### `ClientOptionsBuilder::poll_interval_seconds`

```rust
pub fn poll_interval_seconds(&mut self, value: u64) -> &mut Self
```

Interval for polling flag definitions, in seconds. Defaults to `30`.

#### `ClientOptionsBuilder::disabled`

```rust
pub fn disabled(&mut self, value: bool) -> &mut Self
```

Disable tracking and remote flag requests. Useful for development and
tests.

#### `ClientOptionsBuilder::disable_geoip`

```rust
pub fn disable_geoip(&mut self, value: bool) -> &mut Self
```

Disable automatic GeoIP enrichment for capture and flag requests.

#### `ClientOptionsBuilder::is_server`

```rust
pub fn is_server(&mut self, value: bool) -> &mut Self
```

Whether events originate from a server-side runtime. Defaults to `true`,
which stamps `$is_server: true` so PostHog won't attribute the host OS to
the user. Set `false` for client/CLI use (the property is then omitted).

#### `ClientOptionsBuilder::feature_flags_request_timeout_seconds`

```rust
pub fn feature_flags_request_timeout_seconds(&mut self, value: u64) -> &mut Self
```

Per-request timeout in seconds for remote `/flags` requests. Defaults to `3`.
Also applies to custom HTTP clients, replacing their request timeout even
when that timeout is shorter. Capture and local-evaluation polling use
`request_timeout_seconds`.

#### `ClientOptionsBuilder::feature_flags_request_max_retries`

```rust
pub fn feature_flags_request_max_retries(&mut self, value: u32) -> &mut Self
```

Maximum number of retries after a transient remote `/flags` failure
(transport error or HTTP 502/504). Defaults to `1`. Set to `0` to
disable retries.

#### `ClientOptionsBuilder::error_tracking`

```rust
pub fn error_tracking(&mut self, value: ErrorTrackingOptions) -> &mut Self
```

Error tracking stacktrace and frame classification options

#### `ClientOptionsBuilder::local_evaluation_only`

```rust
pub fn local_evaluation_only(&mut self, value: bool) -> &mut Self
```

When true, never fall back to the remote API for flag evaluation. If local
evaluation is inconclusive (flag not cached or missing properties), the SDK
returns `Ok(None)` instead of making a network call. Only meaningful when
`enable_local_evaluation` is also true.

#### `ClientOptionsBuilder::max_capture_attempts`

```rust
pub fn max_capture_attempts(&mut self, value: u32) -> &mut Self
```

Maximum number of attempts for capture requests (default: 4).
Includes the initial attempt, so `4` means 1 initial + 3 retries.

#### `ClientOptionsBuilder::retry_initial_backoff_ms`

```rust
pub fn retry_initial_backoff_ms(&mut self, value: u64) -> &mut Self
```

Initial capture retry backoff duration in milliseconds (default: 100).
Remote `/flags` retries use their own fixed backoff.

#### `ClientOptionsBuilder::retry_max_backoff_ms`

```rust
pub fn retry_max_backoff_ms(&mut self, value: u64) -> &mut Self
```

Maximum capture retry backoff duration in milliseconds (default: 30000)

#### `ClientOptionsBuilder::flush_at`

```rust
pub fn flush_at(&mut self, value: usize) -> &mut Self
```

Number of buffered events that triggers an automatic flush (default: 100).

#### `ClientOptionsBuilder::max_batch_size`

```rust
pub fn max_batch_size(&mut self, value: usize) -> &mut Self
```

Maximum number of events sent in a single batch request (default: 100).
A flush of more than this many events is split into multiple requests.

#### `ClientOptionsBuilder::flush_interval_ms`

```rust
pub fn flush_interval_ms(&mut self, value: u64) -> &mut Self
```

Interval between automatic time-based flushes, in milliseconds
(default: 5000).

#### `ClientOptionsBuilder::max_queue_size`

```rust
pub fn max_queue_size(&mut self, value: usize) -> &mut Self
```

Maximum number of events buffered before new events are dropped
(default: 10000). A single warning is logged while the queue is full.

#### `ClientOptionsBuilder::shutdown_timeout_ms`

```rust
pub fn shutdown_timeout_ms(&mut self, value: u64) -> &mut Self
```

Maximum time `shutdown()` and `Drop` spend draining buffered and
retrying events before abandoning the rest, in milliseconds (default:
30000). This bounds the drain itself, including any delivery the drain
starts. It does not bound work already underway: each background
worker (analytics, and the AI lane once used) performs one blocking send
at a time, so an automatic flush or drain in progress when shutdown is
requested runs to completion first — up to `request_timeout_seconds`
per in-flight batch, including with a custom blocking client, so a large
auto-drain can delay teardown by several request timeouts. The lanes
drain concurrently, so the budget is shared, not doubled. `flush()` is
unaffected.

#### `ClientOptionsBuilder::capture_compression`

```rust
pub fn capture_compression(&mut self, value: CaptureCompression) -> &mut Self
```

Optional request-body compression. When `None` (default), bodies are
sent uncompressed. The capture endpoint supports all variants.

#### `ClientOptionsBuilder::capture_ai_compression`

```rust
pub fn capture_ai_compression(&mut self, value: CaptureCompression) -> &mut Self
```

Compression for `capture_ai*` bodies; unset (default) sends them
uncompressed. Prefer `Zstd`: AI events are large JSON.

#### `ClientOptionsBuilder::capture_ai_max_queue_size`

```rust
pub fn capture_ai_max_queue_size(&mut self, value: usize) -> &mut Self
```

Maximum `capture_ai*` events buffered before new ones are dropped
(default: 1000). Lower than `max_queue_size` because AI events can be
multi-MB; a single warning is logged while the queue is full.

#### `ClientOptionsBuilder::before_send`

```rust
pub fn before_send<F>(&mut self, hook: F) -> &mut Self
where
    F: FnMut(Event) -> Option<Event> + Send + 'static,
```

Add a hook that can modify or discard events before they are sent.

Hooks should avoid panicking. Panics are caught and drop the current event,
but any mutable state captured by the hook may be left partially updated
and will be reused on subsequent calls.

#### `ClientOptionsBuilder::on_error`

```rust
pub fn on_error<F>(&mut self, hook: F) -> &mut Self
where
    F: Fn(&PostHogError<'_>) + Send + Sync + 'static,
```

Add a hook invoked once per terminal failure on an SDK network surface.

The hook receives a [`PostHogError`] for a capture batch the SDK gave up
delivering, a failed remote `/flags` request, or a failed
local-evaluation poll. Multiple hooks fire in registration order.

##### Observability only — never emit from the hook

The hook MUST NOT call back into the SDK (`capture`/`capture_batch`/
`capture_exception`, `flush`, or `shutdown`): emitting an event while
handling a capture failure forms an amplification loop. The hook is
`Fn + Send + Sync` and invoked without holding any SDK lock, so it may
run concurrently on multiple threads and must be internally thread-safe.
Keep it cheap and non-blocking; the capture hook runs on the background
transport thread. Panics are caught and ignored.

Registering a hook silences the default WARN for terminal capture
reject/exhaustion and serialization failures (the caller now owns that
signal). Shutdown-timeout, queue-full, and `before_send` drops, and
retryable failures that shutdown stops from being retried, keep their
WARN logs and do **not** fire the hook. A batch that has used all
`max_capture_attempts` still fires it.
The existing `/flags` and poller WARN logs are unaffected.

[`PostHogError`]: #posthogerror

#### `ClientOptionsBuilder::build`

```rust
pub fn build(&self) -> Result<ClientOptions, ClientOptionsBuilderError>
```

Build sanitized [`ClientOptions`].

Missing, whitespace-only and personal (`phx_...`) API keys are allowed and
disable the client with one error log, so SDK initialization remains
infallible and never sends a key PostHog would reject.

##### Errors

Returns [`ClientOptionsBuilderError`] if a required builder value is
invalid according to the generated builder.

[`ClientOptionsBuilderError`]: #clientoptionsbuildererror
[`ClientOptions`]: #clientoptions

### `ClientOptionsBuilderError`

```rust
pub enum ClientOptionsBuilderError
```

**Variants**

- `UninitializedField(&'static str)` - Uninitialized field
- `ValidationError(::derive_builder::export::core::string::String)` - Custom validation error

Error type for ClientOptionsBuilder

### `CaptureFailure`

```rust
pub struct CaptureFailure<'a>
```

Details of a terminal capture batch failure.

Fields are read through accessors; the struct is `#[non_exhaustive]`.

Does not fire for shutdown-timeout, queue-full, `before_send`, or local
oversize-AI-event drops — those are not delivery failures.

#### `CaptureFailure::endpoint`

```rust
pub fn endpoint(&self) -> Endpoint
```

The capture endpoint the failed batch targeted: [`Endpoint::Capture`]
for `capture`/`capture_batch`, [`Endpoint::CaptureAi`] for
`capture_ai`/`capture_ai_batch`. Lets one hook tell the lanes apart.

[`Endpoint::Capture`]: #endpointcapture
[`Endpoint::CaptureAi`]: #endpointcaptureai

#### `CaptureFailure::error`

```rust
pub fn error(&self) -> Option<&Error>
```

The batch-level cause: a permanent reject, exhausted transport/HTTP
retries, or a serialization failure.
`None` only when the request itself succeeded (`2xx`) but some events were not
persisted after the retry budget — inspect [`event_results`](Self::event_results).

[Self::event_results]: #selfevent_results

#### `CaptureFailure::status`

```rust
pub fn status(&self) -> Option<u16>
```

The HTTP status of the final attempt, or `None` when no response was
received (a transport error or a serialization failure before sending).

#### `CaptureFailure::attempt`

```rust
pub fn attempt(&self) -> u32
```

The failing attempt number (equals the configured maximum on exhaustion).

#### `CaptureFailure::event_count`

```rust
pub fn event_count(&self) -> usize
```

Number of events this failure dropped (lost).
Counts only undelivered events (`retry`/`drop`), including any finalized on
earlier attempts. This can be smaller than [`event_results`](Self::event_results)`.len()`,
which also reports persisted `ok`/`warning` verdicts — filter by status before
treating an entry as lost.

It also counts each copy capture dropped because an earlier event in the
same request had its uuid, as reported by an entry's
[`duplicates_dropped`](EventResult::duplicates_dropped).

[Self::event_results]: #selfevent_results
[EventResult::duplicates_dropped]: #eventresultduplicates_dropped

#### `CaptureFailure::historical_migration`

```rust
pub fn historical_migration(&self) -> bool
```

Whether the batch was a historical-migration batch.

#### `CaptureFailure::request_id`

```rust
pub fn request_id(&self) -> Option<&Uuid>
```

The capture `posthog-request-id` of the final attempt, when one was
sent. `None` for a serialization failure where no request reached the
wire.

#### `CaptureFailure::event_results`

```rust
pub fn event_results(&self) -> &HashMap<Uuid, EventResult>
```

Per-event server verdicts for the capture batch.

Maps event UUID to its [`EventResult`]. Includes **all** verdicts the
batch collected — persisted (`ok`/`warning`) as well as lost
(`retry`/`drop`) — so this map can be larger than
[`event_count`](Self::event_count); filter by
[`EventStatus`](crate::EventStatus) to isolate the failures. Complete
when [`error`](Self::error) is `None` (a `2xx` where events weren't
persisted after retries); possibly partial on a batch-level failure
(only verdicts collected from earlier attempts).

An entry's [`duplicates_dropped`](EventResult::duplicates_dropped) counts
lost copies that shared its uuid; they have no entry of their own.

[Self::event_count]: #selfevent_count
[crate::EventStatus]: #crateeventstatus
[EventResult::duplicates_dropped]: #eventresultduplicates_dropped
[Self::error]: #selferror
[`EventResult`]: #eventresult

#### `CaptureFailure::verdict_summary`

```rust
pub fn verdict_summary(&self) -> String
```

One line with every `drop` and `retry` reason in the batch and its count, drops first
and most frequent first, for example `drop/invalid_options=2, retry/not_persisted=1`,
or an empty string when nothing was lost. Dropped duplicate copies show as
`drop/duplicate_event_uuid`.

Reasons come from the server, so the summary strips control characters and line
separators, cuts each reason to 64 characters, and shows a missing one as `unspecified`.

```rust
use posthog::{ClientOptionsBuilder, PostHogError};

let options = ClientOptionsBuilder::default()
    .api_key("phc_example".to_string())
    .on_error(|failure| {
        if let PostHogError::Capture(capture) = failure {
            eprintln!(
                "{} event(s) lost: {}",
                capture.event_count(),
                capture.verdict_summary()
            );
        }
    })
    .build()
    .unwrap();
```

#### `CaptureFailure::error_response`

```rust
pub fn error_response(&self) -> Option<&CaptureErrorResponse>
```

The structured error body returned by the capture backend on a
non-`2xx` response (`error`, `error_description`, `error_uri`), when the
body parsed as one. `None` for a transport error, a `2xx`, or an
unrecognizable body — the raw body remains available via
[`error`](Self::error).

[Self::error]: #selferror

### `FlagsFailure`

```rust
pub struct FlagsFailure<'a>
```

Details of a failed remote `/flags` request.

Fields are read through accessors; the struct is `#[non_exhaustive]`.

#### `FlagsFailure::error`

```rust
pub fn error(&self) -> &Error
```

The cause of the failure ([`Error::Connection`] for transport or
non-success status, [`Error::Serialization`] for an unparseable body).

[`Error::Serialization`]: #errorserialization
[`Error::Connection`]: #errorconnection

#### `FlagsFailure::endpoint`

```rust
pub fn endpoint(&self) -> &str
```

The `/flags` endpoint URL the request targeted.

#### `FlagsFailure::distinct_id`

```rust
pub fn distinct_id(&self) -> Option<&str>
```

The `distinct_id` the request was evaluating flags for, when known.

#### `FlagsFailure::status`

```rust
pub fn status(&self) -> Option<u16>
```

The HTTP status, or `None` when no response was received (a transport
error or an exhausted transient-retry budget).

#### `FlagsFailure::body`

```rust
pub fn body(&self) -> Option<&str>
```

The response body, when one was read (present on a non-success status).

### `LocalEvaluationFailure`

```rust
pub struct LocalEvaluationFailure<'a>
```

Details of a failed local-evaluation definitions poll.

Fields are read through accessors; the struct is `#[non_exhaustive]`.
Credentials (the personal API key) are never surfaced here.

#### `LocalEvaluationFailure::error`

```rust
pub fn error(&self) -> &Error
```

The cause of the failure ([`Error::Connection`] for transport or
non-success status, [`Error::Serialization`] for an unparseable body).

[`Error::Serialization`]: #errorserialization
[`Error::Connection`]: #errorconnection

#### `LocalEvaluationFailure::status`

```rust
pub fn status(&self) -> Option<u16>
```

The HTTP status, or `None` when no response was received (a transport
error).

### `PostHogError`

```rust
pub enum PostHogError<'a>
```

**Variants**

- `Capture(CaptureFailure<'a>)` - A capture batch the SDK gave up delivering (permanent reject, exhausted retries, serialization failure) or — on the V1 pipeline — a `2xx` whose per-event verdicts left events unpersisted after the retry budget.
- `FeatureFlags(FlagsFailure<'a>)` - A remote `/flags` request that failed (transport error, non-success status, or an unparseable response body).
- `LocalEvaluation(LocalEvaluationFailure<'a>)` - A background local-evaluation definitions poll that failed (transport error, non-success status, or an unparseable response body). The SDK keeps serving the previously cached definitions.

A terminal failure on one of the SDK's network surfaces, passed by reference
to each registered `on_error` hook.

`#[non_exhaustive]`: new variants may be added as more surfaces gain hook
coverage, so a `match` must include a wildcard arm.

### `Endpoint`

```rust
pub enum Endpoint
```

**Variants**

- `Capture` - Analytics event capture endpoint (`capture`, `capture_batch`, and their immediate variants).
- `CaptureAi` - AI event capture endpoint (`capture_ai`, `capture_ai_batch`, and their immediate variants). A separate lane with its own size limits.
- `Flags` - Feature flags endpoint
- `LocalEvaluation` - Local evaluation endpoint

API endpoints used by the SDK for different operations.

`#[non_exhaustive]`: new endpoints can be added without breaking callers.

#### `Endpoint::path`

```rust
pub fn path(&self) -> &str
```

Get the URL path for this endpoint.

### `CodeEmail`

```rust
pub struct CodeEmail
```

One email whose content your code rendered, such as React Email output,
named by a key. PostHog sends it as is.

It needs [`html`](Self::html), [`text`](Self::text) or both. Without
`text`, PostHog derives it from the `html`. To send a React email, render
it to HTML in your app and pass the HTML.

```rust
use posthog::CodeEmail;

let email = CodeEmail::new("jane@example.com", "password-reset", "Reset your password")
    .html(rendered_html)
    .from("Acme <security@example.com>")
    .distinct_id("user_123");
```

[Self::text]: #selftext
[Self::html]: #selfhtml

#### `CodeEmail::new`

```rust
pub fn new(to: impl Into<String>, template_key: impl Into<String>, subject: impl Into<String>) -> Self
```

An email to one recipient address, with a subject.

`template_key` names the email, such as `"password-reset"`, so its
metrics, logs and status group under it in PostHog. It takes letters,
digits, hyphens and underscores, at most 400.

#### `CodeEmail::html`

```rust
pub fn html(self, html: impl Into<String>) -> Self
```

The HTML body.

#### `CodeEmail::text`

```rust
pub fn text(self, text: impl Into<String>) -> Self
```

The plain-text body.

#### `CodeEmail::from`

```rust
pub fn from(self, from: impl Into<String>) -> Self
```

A verified sender, such as `"Acme <security@example.com>"`. Without
it, PostHog uses the project's default sender.

#### `CodeEmail::reply_to`

```rust
pub fn reply_to(self, address: impl Into<String>) -> Self
```

Adds a reply-to address. Call it again to add more.

#### `CodeEmail::capture_engagement`

```rust
pub fn capture_engagement(self, capture: bool) -> Self
```

Whether PostHog captures the email's engagement events (sent,
delivered, opened, clicked, bounced) as events of the recipient.
Defaults to `true`. A project whose admin turned engagement capture off
captures nothing, whatever the send asks.

#### `CodeEmail::distinct_id`

```rust
pub fn distinct_id(self, distinct_id: impl Into<String>) -> Self
```

The recipient's distinct ID in your product. The email's events go to
this person. Without it, they go to a person whose distinct ID is the
email address.

#### `CodeEmail::idempotency_key`

```rust
pub fn idempotency_key(self, key: impl Into<String>) -> Self
```

1 to 255 visible ASCII characters.

A send with a key that PostHog already accepted queues nothing and
returns the first send's result. The same key for another email returns
`idempotency_key_reused`. Without a key, the client makes a UUID that
covers its own retries of this call. Pass a key derived from what the
email is for, such as `password-reset:user_123:<request id>`, to cover
retries outside this call too.

#### `CodeEmail::max_retries`

```rust
pub fn max_retries(self, max_retries: u32) -> Self
```

Overrides the client's `email_request_max_retries` for this send. `0`
turns retries off.

### `EmailDeliveryStatus`

```rust
pub enum EmailDeliveryStatus
```

**Variants**

- `Queued` - The email waits to go out.
- `Sent` - PostHog handed the email to the email provider.
- `Failed` - The email was not sent, and the status's `reason` says why.
- `Delivered` - The recipient's mail server accepted the email.
- `Bounced` - The recipient's mail provider reported a bounce after delivery.
- `Complained` - The recipient marked the email as spam.
- `Other(String)` - A status this SDK version doesn't know, as PostHog sent it.

What happened to an email. Later SDK versions can know statuses that this
one passes through as [`Other`](Self::Other).

[Self::Other]: #selfother

#### `EmailDeliveryStatus::as_str`

```rust
pub fn as_str(&self) -> &str
```

The status as PostHog sends it, such as `"delivered"`.

### `EmailError`

```rust
pub struct EmailError
```

PostHog did not queue the email or read its status, or the client could not
tell whether it did.

Branch on [`code`](Self::code), PostHog's stable error code such as
`quota_limited`, or on [`kind`](Self::kind), a coarse category. The message
never holds the secret key or the request body.

[Self::kind]: #selfkind
[Self::code]: #selfcode

#### `EmailError::kind`

```rust
pub fn kind(&self) -> EmailErrorKind
```

The coarse category of the failure.

#### `EmailError::code`

```rust
pub fn code(&self) -> &str
```

PostHog's error code, such as `quota_limited` or `recipient_suppressed`,
or an SDK code, such as `network_error`, `missing_secret_key` or
`invalid_template_id`.

#### `EmailError::message`

```rust
pub fn message(&self) -> &str
```

What went wrong, in words.

#### `EmailError::status`

```rust
pub fn status(&self) -> Option<u16>
```

The HTTP status, when PostHog answered.

#### `EmailError::why`

```rust
pub fn why(&self) -> Option<&str>
```

Why PostHog refused the request, when it said.

#### `EmailError::fix`

```rust
pub fn fix(&self) -> Option<&str>
```

How to fix the request, when PostHog or the SDK said.

#### `EmailError::docs_url`

```rust
pub fn docs_url(&self) -> Option<&str>
```

A link to the docs for this error, when PostHog gave one.

#### `EmailError::field`

```rust
pub fn field(&self) -> Option<&str>
```

The request field the error is about, such as `to`, when one is known.

#### `EmailError::retry_after`

```rust
pub fn retry_after(&self) -> Option<Duration>
```

How long PostHog asked the client to wait before it sends again, from
the `Retry-After` header of a `429` or `503`.

### `EmailErrorKind`

```rust
pub enum EmailErrorKind
```

**Variants**

- `ValidationError` - The send was malformed: a local argument check failed, or PostHog answered `400`, a `409` with `template_outdated`, or a `422` other than a reused idempotency key.
- `MissingApiKey` - The client has no secret key, or PostHog answered `401`.
- `InvalidPermission` - PostHog answered `403`: the secret key lacks the scope.
- `NotFound` - PostHog answered `404`: no such template or email in this project.
- `ConcurrentIdempotentRequests` - PostHog answered `409`: a send with the same idempotency key is still in flight.
- `InvalidIdempotentRequest` - PostHog answered `422` with `idempotency_key_reused`: the key was already used for a different email.
- `RateLimitExceeded` - PostHog answered `429`: too many requests.
- `MonthlyQuotaExceeded` - PostHog answered `429` with `quota_limited`: the monthly quota is spent.
- `ServiceUnavailable` - PostHog answered `503`, or the request failed on the network.
- `ApplicationError` - Anything else.

A coarse category of an [`EmailError`], for code that branches on the kind
of failure rather than on the exact [`code`](EmailError::code).

[`EmailError`]: #emailerror
[EmailError::code]: #emailerrorcode

#### `EmailErrorKind::as_str`

```rust
pub fn as_str(self) -> &'static str
```

The kind's name, such as `"validation_error"`, as every PostHog SDK
spells it.

### `EmailStatus`

```rust
pub struct EmailStatus
```

**Fields**

- `id: String` - The message ID that `send` returned.
- `status: EmailDeliveryStatus` - What happened to the email.
- `created_at: chrono::DateTime<chrono::Utc>` - When PostHog queued the email.
- `status_at: chrono::DateTime<chrono::Utc>` - When the email reached its current status.
- `reason: Option<String>` - Why a failed email was not sent, such as `provider_error` or `recipient_suppressed`, or the bounce type of a bounced email: `hard`, `soft` or `unknown`. `None` otherwise.

The status of an email that [`Emails::send`](crate::Emails::send) queued.

[crate::Emails::send]: #crateemailssend

### `EmailWarning`

```rust
pub struct EmailWarning
```

**Fields**

- `code: String` - The warning's stable code.
- `message: String` - What the warning is about, in words.
- `fix: Option<String>` - How to fix it, when PostHog said.

A warning PostHog returned with a queued email.

### `Emails`

```rust
pub struct Emails<'a>
```

Transactional email on a [`Client`](crate::Client): sends email from
templates made in PostHog or from content your code rendered, and reads the
status of each email it sent. Get it with
[`Client::emails`](crate::Client::emails).

#### Setup

1. Create a project secret API key (`phs_...`) with the "Send transactional
   email" preset (`transactional_email:write`) in your project settings, and
   set it as the client's `secret_key`. Add `transactional_email:read` too if
   you read email statuses with a key that can't send. Keys made with
   `hog_flow:write` still work. The project API key (`phc_...`) captures
   events but can't send email.
2. On PostHog Cloud, set `host` to your region, such as
   `https://eu.i.posthog.com`, and the client sends email to that region's
   app host. For any other `host`, such as a reverse proxy, set `app_host`
   to your PostHog app host: the proxy must not receive the secret key.
3. To send a template made in PostHog, open it under Workflows, make it
   transactional and copy its ID.

A result means PostHog queued the email. Delivery happens later: read it
with [`Emails::get`].

#### Timeouts and retries

Each request times out after `email_request_timeout_seconds`, 15 by
default. A request retries network errors, rate limits, a send still in
flight and an unavailable service `email_request_max_retries` times, 2 by
default, after about 1 and then 2 seconds, or after the server's
`Retry-After` when that is longer. A `Retry-After` over 30 seconds fails at
once. Every attempt of a send carries the same idempotency key, so a retry
never sends twice. When a job runner already retries the send, set
`max_retries(0)` on it and pass an idempotency key that names the email.

#### Moving from another email provider

Set `migrated_from`, such as `"resend"`, on the client, so PostHog knows
where your sends came from.

[crate::Client]: #crateclient
[`Emails::get`]: #emailsget
[crate::Client::emails]: #crateclientemails

#### `Emails::send`

```rust
pub async fn send(&self, email: impl Into<SendEmail>) -> Result<SendEmailResult, EmailError>
```

Queues one email, from a template made in PostHog or from content your
code rendered.

`Ok` means PostHog queued the email, not that it was delivered. Network
errors, rate limits, a send with the same idempotency key still in
flight and an unavailable service are retried with the same idempotency
key.

##### Errors

Returns an [`EmailError`] when an argument check fails, the client is
disabled, shut down or has no `secret_key`, or PostHog did not queue the
email.

##### Examples

```rust
use posthog::{ClientOptionsBuilder, CodeEmail, TemplateEmail};
use serde_json::json;

let options = ClientOptionsBuilder::default()
    .api_key("phc_...".to_string())
    .secret_key("phs_...")
    .host("https://eu.i.posthog.com")
    .build()
    .unwrap();
let posthog = posthog::client(options).await;

// A template made in PostHog.
let sent = posthog
    .emails()
    .send(
        TemplateEmail::new("jane@example.com", "0190f6d2-7c1e-7b3a-9f5e-1c2d3e4f5a6b")
            .template_data(json!({ "name": "Jane" }))
            .distinct_id("user_123")
            .idempotency_key("welcome:user_123"),
    )
    .await?;
println!("queued {}", sent.id);

// Content your code rendered, such as React Email output.
posthog
    .emails()
    .send(
        CodeEmail::new("jane@example.com", "password-reset", "Reset your password")
            .html(rendered_html)
            .distinct_id("user_123"),
    )
    .await?;
```

[`EmailError`]: #emailerror

#### `Emails::get`

```rust
pub async fn get(&self, message_id: &str) -> Result<EmailStatus, EmailError>
```

Reads the status of an email that [`send`](Self::send) queued, for 30
days after the send. A key with only `transactional_email:read` can read
statuses but not send.

##### Errors

Returns an [`EmailError`] when `message_id` is not a message ID, the
client can't reach PostHog, or PostHog answered with an error. The code
is `not_found` when no email of this project has the ID, when the email
is older than 30 days, or for a few seconds after the send.

[Self::send]: #selfsend
[`EmailError`]: #emailerror

### `SendEmail`

```rust
pub enum SendEmail
```

**Variants**

- `Template(TemplateEmail)` - A template made transactional in PostHog.
- `Code(CodeEmail)` - Content your code rendered.

An email for [`Emails::send`](crate::Emails::send): a template made in
PostHog, or content your code rendered.

[crate::Emails::send]: #crateemailssend

### `SendEmailResult`

```rust
pub struct SendEmailResult
```

**Fields**

- `id: String` - The queued email's message ID.
- `replayed: bool` - True when PostHog had already accepted this idempotency key and returned the first send's result.
- `warnings: Vec<EmailWarning>` - Warnings PostHog returned with the email. The client also logs each one.

The queued email.

A result means PostHog queued the email, not that it was delivered. Read
its delivery status with [`Emails::get`](crate::Emails::get).

[crate::Emails::get]: #crateemailsget

### `TemplateEmail`

```rust
pub struct TemplateEmail
```

One email from an email template made transactional in PostHog, sent by
the template's ID.

The template sets its own sender, subject and content.

```rust
use posthog::TemplateEmail;
use serde_json::json;

let email = TemplateEmail::new("jane@example.com", "0190f6d2-7c1e-7b3a-9f5e-1c2d3e4f5a6b")
    .template_data(json!({ "name": "Jane" }))
    .distinct_id("user_123")
    .idempotency_key("welcome:user_123");
```

#### `TemplateEmail::new`

```rust
pub fn new(to: impl Into<String>, template_id: impl Into<String>) -> Self
```

An email to one recipient address from the template with this UUID.

#### `TemplateEmail::template_data`

```rust
pub fn template_data(self, data: Value) -> Self
```

Values for the template's `{{ variables.* }}` placeholders, as a JSON
object. Sent as `message_data`.

#### `TemplateEmail::distinct_id`

```rust
pub fn distinct_id(self, distinct_id: impl Into<String>) -> Self
```

The recipient's distinct ID in your product. The email's events go to
this person. Without it, they go to a person whose distinct ID is the
email address.

#### `TemplateEmail::idempotency_key`

```rust
pub fn idempotency_key(self, key: impl Into<String>) -> Self
```

1 to 255 visible ASCII characters. See [`CodeEmail::idempotency_key`].

[`CodeEmail::idempotency_key`]: #codeemailidempotency_key

#### `TemplateEmail::max_retries`

```rust
pub fn max_retries(self, max_retries: u32) -> Self
```

Overrides the client's `email_request_max_retries` for this send. `0`
turns retries off.

### `Error`

```rust
pub enum Error
```

**Variants**

- `Connection(String)` - Network or HTTP error when communicating with PostHog API
- `Serialization(String)` - Error serializing or deserializing JSON data
- `AlreadyInitialized` - Global client was already initialized via `init_global`
- `NotInitialized` - Global client was not initialized before use
- `PanicHookAlreadyInstalled` - Panic autocapture hook was already installed
- `InvalidTimestamp(String)` - Timestamp could not be parsed or is invalid
- `InconclusiveMatch(String)` - Flag evaluation was inconclusive (e.g., missing required properties, unknown operator)
- `RateLimit` - HTTP 429 — the server is rate limiting requests
- `BadRequest(String)` - HTTP 400 or 413 — the request was malformed or too large
- `ServerError { .. }` - HTTP 5xx — the server encountered an error
- `Unauthorized` - HTTP 401 — invalid or missing Bearer token
- `BillingLimitExceeded(String)` - HTTP 402 — billing quota exceeded (non-retryable)
- `Email(emails::EmailError)` - A transactional email request failed; see [`EmailError`].

Errors that can occur when using the PostHog client.

### `CaptureExceptionOptions`

```rust
pub struct CaptureExceptionOptions
```

Optional context for `capture_exception_with`: person identity, custom
properties, capture options, groups, trace context, and exception
fingerprint/level.

All fields are optional. An empty options set (`new()` / `Default`)
captures the exception personlessly with no extra context. With the
trace-context provider configured on [`ErrorTrackingOptions`], its trace
and span IDs are attached automatically; values set here take precedence.
A `$trace_id` or `$span_id` set with [`CaptureExceptionOptions::property`]
also wins: the provider's IDs are then not added at all.

#### Examples

```rust
use posthog::CaptureExceptionOptions;

let options = CaptureExceptionOptions::new()
    .distinct_id("user-123")
    .property("route", "/checkout")?
    .group("company", "acme")
    .trace_context(
        "00000000000000000000000000000123",
        "0000000000000456",
    )
    .fingerprint("checkout-error")
    .level("warning");
```

[`ErrorTrackingOptions`]: #errortrackingoptions
[`CaptureExceptionOptions::property`]: #captureexceptionoptionsproperty

#### `CaptureExceptionOptions::new`

```rust
pub fn new() -> Self
```

Create an empty options set: personless, no extra context.

#### `CaptureExceptionOptions::distinct_id`

```rust
pub fn distinct_id<S: Into<String>>(self, distinct_id: S) -> Self
```

Associate the exception with a person.

#### `CaptureExceptionOptions::property`

```rust
pub fn property<K: Into<String>, V: Serialize>(self, key: K, value: V) -> Result<Self, Error>
```

Add a custom property to the exception event.

#### `CaptureExceptionOptions::option`

```rust
pub fn option<K: Into<String>, V: Serialize>(self, key: K, value: V) -> Result<Self, Error>
```

Set a capture option on the exception event; see [`Event::insert_option`].

[`Event::insert_option`]: #eventinsert_option

#### `CaptureExceptionOptions::group`

```rust
pub fn group<N: Into<String>, I: Into<String>>(self, group_name: N, group_id: I) -> Self
```

Capture the exception as a group event.

Adding a group does not change person processing. An exception without
a distinct ID is personless, and PostHog attaches groups for group
analytics only to events that process person profiles. Set
`option("process_person_profile", true)` to count it for its groups.

#### `CaptureExceptionOptions::trace_context`

```rust
pub fn trace_context<T: Into<String>, S: Into<String>>(self, trace_id: T, span_id: S) -> Self
```

Associate the exception with a span in a distributed trace.

The IDs are accepted together so an exception can never combine a trace
ID and span ID from different contexts. This pair overrides any context
returned by the client's trace-context provider, and any `$trace_id` or
`$span_id` set with [`CaptureExceptionOptions::property`].

[`CaptureExceptionOptions::property`]: #captureexceptionoptionsproperty

#### `CaptureExceptionOptions::fingerprint`

```rust
pub fn fingerprint<S: Into<String>>(self, fingerprint: S) -> Self
```

Set a custom exception fingerprint.

#### `CaptureExceptionOptions::level`

```rust
pub fn level<S: Into<String>>(self, level: S) -> Self
```

Set the exception severity level. Defaults to `"error"`.

### `ErrorTrackingOptions`

```rust
pub struct ErrorTrackingOptions
```

Client-level Error Tracking configuration, applied to every exception the
client captures. Set it via [`ErrorTrackingOptionsBuilder`] on
`ClientOptions::error_tracking`.

#### Examples

```rust
use posthog::{ErrorTrackingOptionsBuilder, TraceContext};

let options = ErrorTrackingOptionsBuilder::default()
    .capture_stacktrace(true)
    // Substring patterns match file paths and function symbols, so a
    // crate prefix marks that crate's frames as not in-app.
    .in_app_exclude_paths(vec!["other_crate::".to_string()])
    // Read IDs from whichever tracing system the application uses.
    .trace_context_provider(|| {
        Some(TraceContext::new("trace-id", "span-id"))
    })
    .build()
    .unwrap();
```

[`ErrorTrackingOptionsBuilder`]: #errortrackingoptionsbuilder

### `ErrorTrackingOptionsBuilder`

```rust
pub struct ErrorTrackingOptionsBuilder
```

Builder for [`ErrorTrackingOptions`](struct.ErrorTrackingOptions.html).

#### `ErrorTrackingOptionsBuilder::capture_stacktrace`

```rust
pub fn capture_stacktrace(&mut self, value: bool) -> &mut Self
```

Capture a stack trace at the `capture_exception` call site and attach
it to the first entry of `$exception_list` (default: `true`).

The trace shows where the error was *captured*, not where it was
created — a bubbled-up `Err` value carries no stack of its own. The
error type/message chain in `$exception_list` is always sent regardless
of this setting. Disabling it skips the stack walk and per-frame symbol
resolution entirely, which can matter when capturing handled errors in
high-volume paths.

#### `ErrorTrackingOptionsBuilder::in_app_include_paths`

```rust
pub fn in_app_include_paths(&mut self, value: Vec<String>) -> &mut Self
```

Treat only frames matching one of these patterns as in-app. Patterns
are substring matches against a frame's file path *and* function
symbol, so both path fragments (`"/service/"`) and crate prefixes
(`"my_service::"`) work. When empty, built-in defaults apply: frames
from the cargo registry, the standard library, and vendored/target
paths are library frames, everything else is in-app.

#### `ErrorTrackingOptionsBuilder::in_app_exclude_paths`

```rust
pub fn in_app_exclude_paths(&mut self, value: Vec<String>) -> &mut Self
```

Always mark matching frames as not in-app, taking precedence over
includes and defaults. Same matching rules as `in_app_include_paths`
— e.g. `"other_crate::"` excludes every frame of that crate.

#### `ErrorTrackingOptionsBuilder::capture_panics`

```rust
pub fn capture_panics(&mut self, value: bool) -> &mut Self
```

When `true`, [`crate::init_global`] installs a process-wide panic hook
that captures panics as `$exception` events through the global client.
Defaults to `false` — panic autocapture is opt-in.

Only the global client installs a hook: a panic hook is process-global
(`std::panic::set_hook`), so it pairs with the process-global client, and
there is intentionally no per-`Client` panic API for now.

[`crate::init_global`]: #crateinit_global

#### `ErrorTrackingOptionsBuilder::build`

```rust
pub fn build(&self) -> ::derive_builder::export::core::result::Result<ErrorTrackingOptions, ErrorTrackingOptionsBuilderError>
```

Builds a new `ErrorTrackingOptions`.

##### Errors

If a required field has not been initialized.

#### `ErrorTrackingOptionsBuilder::trace_context_provider`

```rust
pub fn trace_context_provider<F>(&mut self, provider: F) -> &mut Self
where
    F: Fn() -> Option<TraceContext> + Send + Sync + 'static,
```

Set a callback that returns the distributed trace context active at an
exception's capture site.

The callback runs synchronously during `capture_exception`. It must be
fast, non-blocking, and must not call back into the PostHog SDK. Panics
are caught and cause trace context to be omitted from that exception.
The callback is skipped for an exception that sets a `$trace_id` or
`$span_id` property or an explicit
[`CaptureExceptionOptions::trace_context`].

[`CaptureExceptionOptions::trace_context`]: #captureexceptionoptionstrace_context

### `ErrorTrackingOptionsBuilderError`

```rust
pub enum ErrorTrackingOptionsBuilderError
```

**Variants**

- `UninitializedField(&'static str)` - Uninitialized field
- `ValidationError(::derive_builder::export::core::string::String)` - Custom validation error

Error type for ErrorTrackingOptionsBuilder

### `TraceContext`

```rust
pub struct TraceContext
```

An atomic trace/span identifier pair used to correlate an exception with
distributed traces and logs.

#### `TraceContext::new`

```rust
pub fn new<T: Into<String>, S: Into<String>>(trace_id: T, span_id: S) -> Self
```

Create a trace context from its trace and span identifiers.

### `Event`

```rust
pub struct Event
```

An [`Event`] represents an interaction a user has with your app or
website. Examples include button clicks, pageviews, query completions, and signups.
See the [PostHog documentation](https://posthog.com/docs/data/events)
for a detailed explanation of PostHog Events.

[`Event`]: #event

#### `Event::insert_prop`

```rust
pub fn insert_prop<K: Into<String>, P: Serialize>(&mut self, key: K, prop: P) -> Result<(), Error>
```

Add a property to the event.

##### Parameters

- `key`: Property name.
- `prop`: Any value that can be serialized to JSON.

##### Errors

Returns [`Error::Serialization`] if `prop` cannot be serialized.

[`Error::Serialization`]: #errorserialization

#### `Event::remove_prop`

```rust
pub fn remove_prop(&mut self, key: &str) -> Option<serde_json::Value>
```

Remove a property from the event and return its previous value, if any.

#### `Event::insert_option`

```rust
pub fn insert_option<K: Into<String>, V: Serialize>(&mut self, key: K, value: V) -> Result<(), Error>
```

Set a capture option that tells PostHog how to process the event, such
as `"process_person_profile"`.

##### Parameters

- `key`: Option name.
- `value`: Any value that can be serialized to JSON.

##### Errors

Returns [`Error::Serialization`] if `value` cannot be serialized.

##### Remarks

Sent unchecked: PostHog ignores unknown keys and drops the event if a
known key has an unusable value. Legacy option properties still work,
but an option overrides its legacy property. Use options.

[`Error::Serialization`]: #errorserialization

#### `Event::remove_option`

```rust
pub fn remove_option(&mut self, key: &str) -> Option<serde_json::Value>
```

Remove a capture option from the event and return its previous value, if any.

#### `Event::add_group`

```rust
pub fn add_group(&mut self, group_name: &str, group_id: &str)
```

Capture this as a group event.

See <https://posthog.com/docs/product-analytics/group-analytics#how-to-capture-group-events>.

##### Parameters

- `group_name`: Group type, such as `"company"`.
- `group_id`: Stable identifier for the group.

##### Remarks

Adding a group does not change person processing. PostHog attaches
groups for group analytics only to events that process person profiles,
so an event from [`Event::new_anon`] needs
`insert_option("process_person_profile", true)` to count for its groups.

[`Event::new_anon`]: #eventnew_anon

#### `Event::set_timestamp`

```rust
pub fn set_timestamp<Tz>(&mut self, timestamp: DateTime<Tz>) -> Result<(), Error>
where
    Tz: TimeZone,
```

Set the event timestamp, for events that happened in the past.

##### Parameters

- `timestamp`: Timestamp to send with the event. UTC input is preferred;
  non-UTC input is converted to the equivalent UTC instant before serialization.

##### Errors

Never returns an error; PostHog clamps timestamps far in the future.
The `Result` stays so existing callers keep compiling.

#### `Event::set_uuid`

```rust
pub fn set_uuid(&mut self, uuid: Uuid)
```

Override the auto-generated UUID for this event.

Useful for deduplication when re-importing historical data. Every
capture of an event with this uuid sends the same uuid. When one request
contains a uuid twice, PostHog keeps the first event and drops the later
copies, which the SDK reports with the reason `duplicate_event_uuid`.

#### `Event::with_flags`

```rust
pub fn with_flags(&mut self, flags: &FeatureFlagEvaluations) -> &mut Self
```

Attach the flag state captured by a [`FeatureFlagEvaluations`] snapshot
to this event.

Adds `$feature/<key>` for every evaluated flag plus a sorted
`$active_feature_flags` list of enabled keys, mirroring what
`send_feature_flags` would otherwise fetch — but without making an
extra `/flags` request.

A property already on the event keeps its value, so the caller can
override a `$feature/<key>` or `$active_feature_flags` value whether it
is set before or after this call.

##### Returns

Returns `self` so calls can be chained before capture.

[`FeatureFlagEvaluations`]: #featureflagevaluations

#### `Event::event_name`

```rust
pub fn event_name(&self) -> &str
```

Return the event name.

#### `Event::distinct_id`

```rust
pub fn distinct_id(&self) -> &str
```

Return the event distinct ID.

#### `Event::properties`

```rust
pub fn properties(&self) -> &HashMap<String, serde_json::Value>
```

Return the event properties.

#### `Event::options`

```rust
pub fn options(&self) -> &HashMap<String, serde_json::Value>
```

Return the event's capture options, as set by [`Event::insert_option`].

[`Event::insert_option`]: #eventinsert_option

### `CaptureErrorResponse`

```rust
pub struct CaptureErrorResponse
```

**Fields**

- `error: String`
- `error_description: Option<String>`
- `error_uri: Option<String>`

### `CaptureResponse`

```rust
pub struct CaptureResponse
```

**Fields**

- `results: std::collections::HashMap<uuid::Uuid, EventResult>`

### `EventResult`

```rust
pub struct EventResult
```

**Fields**

- `result: EventStatus`
- `details: Option<String>`
- `duplicates_dropped: Option<u32>` - Number of later copies of this uuid in the same request that capture dropped. `result` and `details` describe the first copy, which capture kept; each dropped copy is lost with the reason `duplicate_event_uuid`.

### `EventStatus`

```rust
pub enum EventStatus
```

**Variants**

- `Ok`
- `Drop`
- `Warning`
- `Retry`
- `Unknown`

Only `Retry` is resent; all other variants are terminal.
`Unknown` (`#[serde(other)]`) is a forward-compat catch-all that deserializes
unrecognised statuses as terminal rather than failing the parse.

### `EvaluateFlagsOptions`

```rust
pub struct EvaluateFlagsOptions
```

**Fields**

- `groups: Option<std::collections::HashMap<String, String>>` - Group keys for group-targeted feature flags, keyed by group type (for example `{ "company": "company_123" }`). These groups are also attached to `$feature_flag_called` events emitted by the returned snapshot.
- `person_properties: Option<std::collections::HashMap<String, serde_json::Value>>` - Person properties used by remote or local flag evaluation. Provide any properties referenced by release conditions when using local evaluation.
- `group_properties: Option<std::collections::HashMap<String, std::collections::HashMap<String, serde_json::Value>>>` - Group properties used by group-targeted or mixed-targeting flags, keyed first by group type and then by property name.
- `only_evaluate_locally: bool` - When `true`, skip the remote `/flags` request and return only locally evaluated results. If local evaluation is not configured, the snapshot is empty.
- `disable_geoip: Option<bool>` - Per-call override for GeoIP behavior on `/flags` and `$feature_flag_called` requests. `None` uses the client-level setting.
- `flag_keys: Option<Vec<String>>` - Optional list of flag keys. When provided, only these flags are evaluated — the underlying `/flags` request asks the server for just this subset, which makes the response smaller and the request cheaper. An explicitly empty list returns an empty snapshot without local or remote evaluation; `None` evaluates all flags.

Optional inputs for [`Client::evaluate_flags`](crate::Client::evaluate_flags).

This struct is non-exhaustive so more options can be added in minor releases.
Create it with [`Default`] and then set the fields you need.

[crate::Client::evaluate_flags]: #crateclientevaluate_flags
[`Default`]: #default

### `CohortDefinition`

```rust
pub struct CohortDefinition
```

**Fields**

- `id: String` - Unique identifier for the cohort.
- `properties: serde_json::Value` - Properties can be either: - A JSON object with "type" and "values" for complex property groups - Or a direct `Vec<Property>` for simple cases

Definition of a cohort for local evaluation

#### `CohortDefinition::new`

```rust
pub fn new(id: String, properties: Vec<Property>) -> Self
```

Create a new cohort definition with a simple property list.

##### Parameters

- `id`: Cohort identifier.
- `properties`: Property filters that define cohort membership.

#### `CohortDefinition::parse_properties`

```rust
pub fn parse_properties(&self) -> Vec<Property>
```

Parse the properties from the JSON structure.

PostHog cohort properties come in format:
`{"type": "AND", "values": [{"type": "property", "key": "...", "value": "...", "operator": "..."}]}`.

### `EvaluationContext`

```rust
pub struct EvaluationContext<'a>
```

**Fields**

- `cohorts: &'a std::collections::HashMap<String, CohortDefinition>` - Cohort definitions available to local evaluation, keyed by cohort ID.
- `flags: &'a std::collections::HashMap<String, FeatureFlag>` - Feature flag definitions available to evaluate flag dependencies, keyed by flag key.
- `distinct_id: &'a str` - Distinct ID used for person-targeted flag bucketing.
- `groups: &'a std::collections::HashMap<String, String>` - Group keys for group-targeted flags, keyed by group type.
- `group_properties: &'a std::collections::HashMap<String, std::collections::HashMap<String, serde_json::Value>>` - Group properties for group-targeted flags, keyed by group type and then property name.
- `group_type_mapping: &'a std::collections::HashMap<String, String>` - Mapping from PostHog group type index to group type name.
- `property_matching_version: i64` - Matching semantics pinned to the definitions snapshot. Exactly `2` uses explicit equality; `1` (and other versions) uses legacy boolean coercion.

Context for evaluating properties that may depend on cohorts or other flags.

`groups`, `group_properties`, and `group_type_mapping` are used to resolve
group-targeted and mixed-targeting flags. A group condition (one whose
`aggregation_group_type_index` is set, either at the flag or condition level)
is bucketed on the group key and matched against group properties looked up
via the group type mapping.

### `FeatureFlag`

```rust
pub struct FeatureFlag
```

**Fields**

- `key: String` - Unique identifier for the flag (e.g., "new-checkout-flow")
- `active: bool` - Whether the flag is currently active. Inactive flags always return false.
- `filters: FeatureFlagFilters` - Targeting rules and rollout configuration
- `has_experiment: Option<bool>` - Whether the flag is linked to an experiment, as reported by the local-evaluation definitions endpoint. Tri-state: `Some(true)`/`Some(false)` when the server reports it, `None` when it does not (older server). Drives the `$feature_flag_has_experiment` property and event minimization.

A feature flag definition from PostHog.

Contains all the information needed to evaluate whether a flag should be
enabled for a given user, including targeting rules and rollout percentages.

### `FeatureFlagCondition`

```rust
pub struct FeatureFlagCondition
```

**Fields**

- `properties: Vec<Property>` - Property filters that must all match (AND logic)
- `rollout_percentage: Option<f64>` - Percentage of matching users who should see this flag (0-100)
- `variant: Option<String>` - Specific variant to serve for this condition (for variant overrides)
- `aggregation_group_type_index: Option<i32>` - Optional per-condition aggregation override used by mixed-targeting flags. When set, this condition targets the specified group type instead of the flag-level aggregation. `None` means person targeting under a mixed flag.

A single condition group within a feature flag's targeting rules.

All properties within a condition must match (AND logic), and the user
must fall within the rollout percentage to be included.

### `FeatureFlagFilters`

```rust
pub struct FeatureFlagFilters
```

**Fields**

- `groups: Vec<FeatureFlagCondition>` - List of condition groups (evaluated with OR logic between groups)
- `multivariate: Option<MultivariateFilter>` - Multivariate configuration for A/B tests with multiple variants
- `payloads: std::collections::HashMap<String, serde_json::Value>` - JSON payloads associated with flag variants
- `aggregation_group_type_index: Option<i32>` - Group type index this flag targets at the flag level. `None` means person targeting (or mixed, when individual conditions set their own).
- `early_exit: bool` - When `true`, local evaluation stops and returns a definitive disabled result as soon as a condition group's property filters match (or it has no property filters) but the rollout percentage excludes the user, instead of falling through to later condition groups. Defaults to `false`, which preserves the legacy fall-through behavior.

Targeting rules and configuration for a feature flag.

### `FeatureFlagsResponse`

```rust
pub enum FeatureFlagsResponse
```

**Variants**

- `V2 { .. }` - v2 API format from `/flags/?v=2` endpoint
- `Legacy { .. }` - Legacy format from older decide endpoint

Response from the PostHog feature flags API.

Supports both the v2 API format (with detailed flag information) and the
legacy format (simple flag values and payloads).

#### `FeatureFlagsResponse::normalize`

```rust
pub fn normalize(self) -> (HashMap<String, FlagValue>, HashMap<String, serde_json::Value>)
```

Convert the response to normalized flag values and payloads.

##### Returns

A tuple of `(feature_flags, feature_flag_payloads)`, each keyed by flag
key.

### `FlagDetail`

```rust
pub struct FlagDetail
```

**Fields**

- `key: String` - The feature flag key
- `enabled: bool` - Whether the flag is enabled for this user
- `variant: Option<String>` - The variant key if this is a multivariate flag
- `reason: Option<FlagReason>` - Reason explaining why the flag evaluated to this value
- `metadata: Option<FlagMetadata>` - Additional metadata about the flag

Detailed information about a feature flag evaluation result.

Returned by the `/flags/?v=2` endpoint with extended information about why a
flag evaluated to a particular value.

### `FlagMetadata`

```rust
pub struct FlagMetadata
```

**Fields**

- `id: u64` - Unique identifier for this flag
- `version: u32` - Version number of the flag definition
- `description: Option<String>` - Optional description of what this flag controls
- `payload: Option<serde_json::Value>` - Optional JSON payload associated with the flag
- `has_experiment: Option<bool>` - Whether the flag is linked to an experiment. Tri-state: `Some(true)`/ `Some(false)` when the `/flags?v=2` metadata reports it, `None` when it is absent (older server or degraded response). Drives the `$feature_flag_has_experiment` property and event minimization.

Metadata about a feature flag from the PostHog server.

### `FlagReason`

```rust
pub struct FlagReason
```

**Fields**

- `code: String` - Reason code (e.g., "condition_match", "out_of_rollout_bound")
- `condition_index: Option<usize>` - Index of the condition that matched (if applicable)
- `description: Option<String>` - Human-readable description of the reason

Explains why a feature flag evaluated to a particular value.

### `FlagValue`

```rust
pub enum FlagValue
```

**Variants**

- `Boolean(bool)` - Flag is either enabled (true) or disabled (false)
- `String(String)` - Flag returns a specific variant key (e.g., "control", "test", "variant-a")

The value of a feature flag evaluation.

Feature flags can return either a boolean (enabled/disabled) or a string
(for multivariate flags where users are assigned to different variants).

### `InconclusiveMatchError`

```rust
pub struct InconclusiveMatchError
```

**Fields**

- `message: String` - Human-readable description of why evaluation was inconclusive

Error returned when a feature flag cannot be evaluated locally.

This typically occurs when:
- Required person/group properties are missing
- A cohort referenced by the flag is not in the local cache
- A dependent flag is not available locally
- An unknown operator is encountered

#### `InconclusiveMatchError::new`

```rust
pub fn new(message: &str) -> Self
```

Create an inconclusive-match error with a human-readable message.

### `MultivariateFilter`

```rust
pub struct MultivariateFilter
```

**Fields**

- `variants: Vec<MultivariateVariant>` - List of variants with their rollout percentages

Configuration for multivariate (A/B/n) feature flags.

### `MultivariateVariant`

```rust
pub struct MultivariateVariant
```

**Fields**

- `key: String` - Unique key for this variant (e.g., "control", "test", "variant-a")
- `rollout_percentage: f64` - Percentage of users who should see this variant (0-100)

A single variant in a multivariate feature flag.

### `Property`

```rust
pub struct Property
```

**Fields**

- `key: String` - The property key to match (e.g., "email", "country", "$feature/other-flag")
- `value: serde_json::Value` - The value to compare against
- `operator: String` - Comparison operator. Supported property operators include `"exact"`, `"is_not"`, `"icontains"`, `"not_icontains"`, `"starts_with"`, `"not_starts_with"`, `"ends_with"`, `"not_ends_with"`, `"regex"`, `"not_regex"`, `"gt"`, `"gte"`, `"lt"`, `"lte"`, `"is_set"`, `"is_not_set"`, `"is_date_before"`, `"is_date_after"`, and the `"semver_*"` operators used by PostHog version targeting.
- `property_type: Option<String>` - Property type. Use `Some("cohort")` for cohort membership checks; flag dependency checks use a property key that starts with `$feature/`.

A property filter used in feature flag targeting.

Supports various operators for matching user properties against expected values.

### `Cohort`

```rust
pub struct Cohort
```

**Fields**

- `properties: serde_json::Value` - The raw cohort property group exactly as returned by the API, e.g. `{"type": "AND", "values": [{"key": "email", "value": "...", ...}]}`. `values` entries may be leaf property filters or nested property groups.

A cohort definition for local evaluation.

Cohorts are groups of users defined by property filters, used for
targeting feature flags to specific user segments.

The `/flags/definitions/?send_cohorts` endpoint maps each cohort ID
straight to its property group, `{"type": "AND"|"OR", "values": [...]}`,
with no wrapping `id`/`name` fields — so this type is transparent over the
raw property group and the owning ID is the map key in
[`LocalEvaluationResponse::cohorts`].

The backend dependency loader can serialize cohort references that form a
cycle across map entries. Serde preserves those references as-is; cohort
evaluation must track active IDs before following them recursively to avoid
a stack overflow.

[`LocalEvaluationResponse::cohorts`]: #localevaluationresponsecohorts

### `FlagCache`

```rust
pub struct FlagCache
```

Thread-safe cache for feature flag definitions.

Stores feature flags, group type mappings, and cohort definitions that have
been fetched from the PostHog API. The cache is shared between the poller
(which updates it) and the evaluator (which reads from it).

#### `FlagCache::new`

```rust
pub fn new() -> Self
```

Create an empty shared flag cache.

#### `FlagCache::update`

```rust
pub fn update(&self, response: LocalEvaluationResponse)
```

Replace cached flags, group type mappings, and cohorts from a local
evaluation API response.

#### `FlagCache::minimal_flag_called_events`

```rust
pub fn minimal_flag_called_events(&self) -> bool
```

Whether the most recent definitions poll enabled minimal
`$feature_flag_called` events. `false` until definitions load, so a
missing signal always yields full events.

#### `FlagCache::get_flag`

```rust
pub fn get_flag(&self, key: &str) -> Option<FeatureFlag>
```

Return a cached feature flag by key.

#### `FlagCache::get_all_flags`

```rust
pub fn get_all_flags(&self) -> Vec<FeatureFlag>
```

Return all cached feature flag definitions.

#### `FlagCache::get_cohort`

```rust
pub fn get_cohort(&self, id: &str) -> Option<Cohort>
```

Return a cached cohort by ID.

#### `FlagCache::get_all_cohorts`

```rust
pub fn get_all_cohorts(&self) -> HashMap<String, Cohort>
```

Return all cached cohorts, keyed by cohort ID.

#### `FlagCache::get_cohort_definitions`

```rust
pub fn get_cohort_definitions(&self) -> HashMap<String, CohortDefinition>
```

Get all cohorts as CohortDefinitions for evaluation context

#### `FlagCache::get_flags_map`

```rust
pub fn get_flags_map(&self) -> HashMap<String, FeatureFlag>
```

Get all flags as a HashMap for evaluation context

#### `FlagCache::get_group_type_mapping`

```rust
pub fn get_group_type_mapping(&self) -> HashMap<String, String>
```

Get the group type mapping (group type index → group type name).

#### `FlagCache::clear`

```rust
pub fn clear(&self)
```

Remove all cached flags, group type mappings, and cohorts.

### `LocalEvaluationConfig`

```rust
pub struct LocalEvaluationConfig
```

**Fields**

- `secret_key: String` - Secret key for authentication.
- `project_api_key: String` - Project API key to identify which project's flags to fetch
- `api_host: String` - PostHog API host URL (for example, `https://us.i.posthog.com`). Use `https://eu.i.posthog.com` for EU-hosted projects.
- `poll_interval: std::time::Duration` - How often to poll for updated flag definitions
- `request_timeout: std::time::Duration` - Timeout for API requests

Configuration for local flag evaluation.

Specifies the credentials and settings needed to fetch feature flag
definitions from the PostHog API for local evaluation.

### `LocalEvaluationResponse`

```rust
pub struct LocalEvaluationResponse
```

**Fields**

- `flags: Vec<feature_flags::FeatureFlag>` - List of feature flag definitions
- `group_type_mapping: std::collections::HashMap<String, String>` - Mapping from group type keys to their display names
- `cohorts: std::collections::HashMap<String, Cohort>` - Cohort definitions for evaluating cohort membership
- `minimal_flag_called_events: bool` - Server-controlled gate: when `true`, `$feature_flag_called` events for non-experiment flags evaluated from these definitions are minimized to a strict property allowlist. Absent fails safe to `false` (full event).
- `property_matching_version: i64` - Property equality semantics for this snapshot. Exactly `2` selects explicit matching; absent metadata and other versions use legacy matching.

Response from the PostHog local evaluation API.

Contains feature flag definitions, group type mappings, and cohort definitions
that can be cached locally for flag evaluation without server round-trips.

#### `LocalEvaluationResponse::new`

```rust
pub fn new(flags: Vec<FeatureFlag>) -> Self
```

Create a local-evaluation response from feature flag definitions.

Group mappings, cohorts, and minimized flag-called events default to
empty or disabled and can be assigned through their public fields.

### `AsyncFlagPoller`

```rust
pub struct AsyncFlagPoller
```

Asynchronous poller for feature flag definitions.

Runs a tokio task that periodically fetches flag definitions from the
PostHog API and updates the shared cache. Use this for async applications.
For blocking/sync applications, use [`FlagPoller`] instead.

[`FlagPoller`]: #flagpoller

#### `AsyncFlagPoller::new`

```rust
pub fn new(config: LocalEvaluationConfig, cache: FlagCache) -> Self
```

Create an asynchronous flag definition poller.

If the HTTP client cannot be built, logs a warning and leaves polling
disabled. [`Self::load_flags`] then returns [`Error::Connection`].

##### Parameters

- `config`: Credentials, host, polling interval, and request timeout.
- `cache`: Shared cache updated by the poller.

[`Self::load_flags`]: #selfload_flags
[`Error::Connection`]: #errorconnection

#### `AsyncFlagPoller::start`

```rust
pub async fn start(&mut self)
```

Start the polling task.

Performs an initial async load, then refreshes definitions in the
background until [`AsyncFlagPoller::stop`] is called or the poller is
dropped.

[`AsyncFlagPoller::stop`]: #asyncflagpollerstop

#### `AsyncFlagPoller::load_flags`

```rust
pub async fn load_flags(&self) -> Result<(), Error>
```

Load flags asynchronously and update the cache once.

##### Errors

Returns [`Error::Connection`] for request failures or non-success HTTP
statuses, and [`Error::Serialization`] when the response cannot be
parsed.

[`Error::Serialization`]: #errorserialization
[`Error::Connection`]: #errorconnection

#### `AsyncFlagPoller::stop`

```rust
pub async fn stop(&mut self)
```

Stop the polling task.

#### `AsyncFlagPoller::is_running`

```rust
pub async fn is_running(&self) -> bool
```

Check if the poller currently has a running background task.

### `PostHogReqwestClient`

```rust
pub struct PostHogReqwestClient
```

An opt-in application HTTP client that sends analytics context to exact allowed hosts.

This client disables automatic redirects and returns redirect responses to the caller.
Reqwest otherwise forwards custom headers across redirects, which can expose analytics IDs.
Configure a separate client for requests that need automatic redirects.

The supplied builder retains its other settings. Do not set PostHog tracing headers as
client default headers: those defaults belong to the application and can apply to any host.
This client does not change the SDK's event ingestion transport.

#### `PostHogReqwestClient::new`

```rust
pub fn new<I, S>(builder: ClientBuilder, allowed_hosts: I) -> Result<Self, reqwest::Error>
where
    I: IntoIterator,
    S: AsRef<str>,
```

Builds a dedicated client with automatic redirects disabled.

Hosts are trimmed and compared without case sensitivity. Each entry must be a hostname
without a scheme, port, path, or wildcard. An empty list disables header injection.

#### `PostHogReqwestClient::client`

```rust
pub fn client(&self) -> &Client
```

Use this client to build requests, then pass them to [`Self::execute`].
Calling reqwest's `send` or `execute` directly does not inject context.

[`Self::execute`]: #selfexecute

#### `PostHogReqwestClient::execute`

```rust
pub async fn execute(&self, request: Request) -> Result<Response, reqwest::Error>
```

Sends a request with the context active when this future is first polled.

Build the request with [`Self::client`] to retain its client configuration.
On allowed hosts, current values replace stale tracing headers. Missing values and
values that cannot form valid HTTP headers are omitted. Other hosts are unchanged.
Spawned tasks can use an explicit [`RequestContext::scope`] to carry context.

[`Self::client`]: #selfclient
[`RequestContext::scope`]: #requestcontextscope

# Stateful client

Enable the optional `stateful` feature for applications that keep local user identity,
person-profile state, consent and a persistent delivery queue. Existing root-level server APIs and defaults
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

## Identity and person profiles

`identify("user-id")` synchronously changes anonymous state to identified and attempts
`$identify` with the previous anonymous distinct ID. The anonymous/device ID is retained.
Identifying the same ID while still anonymous marks it identified and attempts one
`$set` instead. Once identified, another ID is ignored: call `reset()` before switching
users. Repeating the current ID without properties emits nothing. Blank IDs are ignored.

Use `Identify::new(id)` with `set` and `set_once` maps for person properties, or call
`set_person_properties(set, set_once)` for the current identity without identifying.
Empty ordinary property updates do nothing. These APIs share a bounded, process-local
fingerprint of the last accepted update. Object key order does not matter; array order
and explicit nulls do. Dropped or full-queue attempts are not marked as duplicates.
Reset clears this cache; restarting does not preserve it.

`alias(alias)` attempts `$create_alias` without changing the local IDs or identified
status. `create_person_profile()` enables person processing without identifying and
attempts one empty `$set`; it does nothing when processing is already enabled.

`Config::person_profiles` selects:

| Mode | Default capture processing | Identity/profile helper operations |
| --- | --- | --- |
| `IdentifiedOnly` (default) | Enabled after identify, alias, person updates or create-profile | Allowed |
| `Always` | Enabled, including while anonymous | Allowed |
| `Never` | Disabled | Ignored |

Explicit per-event options and hooks retain their normal precedence in every mode;
they change that event, not the client's stored processing state. `is_identified()`
reads the local flag, independently of whether person processing is enabled.

Identity/profile methods return `()`. Their local changes are not atomic with event
admission, persistence or ingestion. A hook drop, full queue or delivery failure does
not roll back identity and does not schedule another linking event. Calling identify
again with the same ID and properties follows the `$set` path, not `$identify` replay.
An already-enabled create-profile call also does not replay a dropped creation event.

`reset()` creates fresh anonymous/distinct/device IDs, clears identified/profile state,
and clears the explicit consent choice (see below). It emits no event and does not purge or rewrite queued events and does not reapply
setup bootstrap. Profile mode is configuration: Always still applies after reset.
Hooks run outside the state lock. A hook may reset, but its outer event keeps the old
snapshot; a delayed capture cannot restore old-generation dedup state after reset.
Recursive emitting identity calls are ignored before changing state. Concurrent calls
share coherent identity snapshots, not a global delivery-order guarantee.

### Identified bootstrap

`Config::bootstrap_identified_id` seeds an identified user with a separate generated
anonymous/device ID in a fresh scope. It is mutually exclusive with
`bootstrap_anonymous_id`; supplying both disables initialization with a diagnostic.

For restored anonymous state, a different identified seed reconciles through identify
before readiness, without waiting for delivery. A matching ID is marked identified
without an identity event. Neither reconciliation runs in Never mode. A fresh identified
seed still initializes identity in Never mode, but capture processing defaults to false.
An already-identified user is retained; a different identified seed produces a warning
rather than silently replacing that user. Anonymous bootstrap remains a fresh-scope seed
and does not replace restored identity.

## Consent

`opt_out()` immediately blocks new collection; `opt_in()` immediately permits it.
Both return `()`, and `is_opt_out()` reads the current choice from memory.
A blocked capture returns `CaptureOutcome::OptedOut` without running hooks.
Disabled or closed clients report opted out and cannot be enabled by opting in.

`Config::opt_out` is the startup default (false). An explicit stored opt-in/out
choice overrides it. Missing or legacy consent uses the configured default.
`reset()` clears the explicit choice and restores that default: with ordinary
configuration, `opt_out(); reset();` enables collection again. With
`Config::opt_out = true`, `opt_in(); reset();` blocks collection again.
Reset before opting in when switching users.

Consent and identity are snapshotted together before hooks. A capture permitted at
that boundary may finish its hooks, queue admission, persistence and delivery after
opt-out. Already queued/in-flight/retrying events and recovered disk records remain
eligible for delivery; opt-out is not a purge or cancellation operation.
A new capture attempted by a hook checks current consent independently.

Identify and identified-bootstrap reconciliation still update local identity while
opted out; their analytics events are blocked. Opt-in retains that identity and
does not replay a dropped linking event. Blocked property events do not populate
the accepted-event deduplication cache. Alias and create-profile calls while opted
out are no-ops; person-property updates retain local processing enablement but their
analytics uses the same consent gate.

Consent persistence uses the existing asynchronous state writer. No consent mutation
waits for disk or network I/O. A successful state drain confirms its captured revision;
before that, a crash can restore an older choice. Write failure reports through
`on_error` without reverting live consent. If stored state cannot be read, the
enabled memory fallback uses the configured startup default and preserves the
unreadable file. Set that default to true if collection must wait for an explicit opt-in.

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

State records use version 5 for identity, profile flags, explicit consent, registered
properties, remembered groups and optional persistent session timing. Valid version-1
identity records restore as anonymous; version-2 records retain identity/profile flags.
Both use the configured consent default; version-3 records retain explicit consent.
Versions 1–3 restore empty registered properties and groups; version 4 retains them.
All legacy versions restore without a session and migrate on the next state write.
The bounded state reserves room for consent changes and session bookkeeping. A migrated
record must fit that bound; otherwise restore reports a storage error and falls back
to fresh memory state without overwriting the file. Older SDKs preserve unsupported
version-5 files rather than rewriting them. Event records and namespace
paths are unchanged. A flush drains the captured state revision, not a transaction
between state and event files.

Initialization does not wait for network connectivity or backlog delivery. Recovery
may temporarily reserve queue capacity after readiness. A ready client can therefore
report `CaptureOutcome::Full`; readiness is not a promise of free queue capacity.

## Registered properties and groups

`register(HashMap<String, Value>)` owns persistent context for future events. New
values replace existing top-level values; null is a value, not removal. Use
`unregister(key)` to remove one entry. Both are local operations available while
opted out and emit no event. A candidate exceeding the state byte or JSON nesting
limits is rejected as a whole, preserving previous context and state durability.
Validation uses the persistence codec in memory, without filesystem access.

`group(type, key, optional_properties)` remembers one membership and attempts
`$groupidentify` on every valid call, including the same key without properties.
`group_identify(type, key, properties)` sends metadata without changing membership.
Both use the current distinct ID, put profile fields under `$group_set`, and enable
person processing without identifying the user. They no-op for opted-out,
`PersonProfiles::Never`, disabled or closed clients. Types and keys must be nonblank
strings. A rejected event does not roll back local membership/processing or schedule
replay. Merely attaching `Capture.groups` does not enable person processing.

`remove_group(type)` removes one membership locally without an event or disabling
processing. It remains available while opted out or configured with Never.
Disabled/closed clients ignore all context mutations. Reset clears registered
properties and remembered groups together with identity and consent state.
Already captured events retain their original context, including through retries
and restart. Hooks can change local context without changing their outer snapshot.

All mutations update memory before returning and use the same asynchronous state
writer. A successful state drain confirms the captured revision; a crash before
persistence may restore older context. Group analytics and state persistence are
independent outcomes. Flag targeting and reloads are not part of these APIs yet.

## Sessions

Sessions are per client, with UUIDv7 IDs. `Config::session_mode` defaults to
`SessionMode::Automatic`: foreground capture/screen attempts and explicit
`session_activity()` create or maintain a session. A session expires at 30 minutes
of inactivity or 24 hours of total age, even with continuous activity. Valid capture
attempts count before hooks and admission; hook drops or full queues do not undo
activity. SDK-generated identity/group events are passive and only attach a valid
existing session. Caller event timestamps do not change session timing.

`session_id()` maintains a session but does not extend an already-valid session's
activity. `peek_session_id()` is a pure read of the stored ID, which may be expired.
Both return `Option<Uuid>`; disabled/closed clients return None. `start_session()`
creates only when absent, `end_session()` clears, and `reset_session()` forces a new
ID. These operations emit no event. In `SessionMode::Manual`, only explicit boundaries
create/clear/rotate sessions; automatic expiry is disabled.

Hosts call `set_app_in_background(bool)`; initial state is foreground. Entering
background touches a still-valid automatic session. Background captures/getters do
not extend it or create a replacement after expiry. Returning to foreground applies
activity policy; duplicate signals do nothing. Identity reset clears the session
atomically while preserving this environmental state. Identification alone does not
rotate it.

While opted out, no session is created, rotated or touched. Maintaining getters return
only valid stored attribution without mutation. Pure peek, explicit end and host
foreground/background bookkeeping remain available. Opt-in alone is not activity.
Already-snapshotted events retain their original identity/session through hooks,
delivery, retries and restart.

### Restart continuity

`Config::session_persistence` defaults to `SessionPersistence::Memory`: a new client
starts sessionless, independently of restored identity. Games can use this default.
Select `SessionPersistence::Persistent` for successive CLI invocations that should
share an unexpired session. Rust saves/restores the ID and timing through the same
state writer, not consumer-managed storage. It respects `Config::persistence`;
an explicitly memory-only backend or storage failure cannot provide durability.

Call flush or shutdown and inspect the state outcome before exiting when continuity
matters. Persistence is asynchronous: a crash can lose recent activity or restore an
older boundary. Selecting Memory ignores previously saved sessions and removes that
session from subsequent state snapshots; it does not rewrite queued event attribution.

Live expiry uses monotonic elapsed time, unaffected by wall-clock changes. OS suspend
accounting follows the platform's monotonic clock. Persistent restart checks use wall
time; expired automatic sessions and future/inconsistent stored timing are not
continued. Manual sessions have no idle/max expiry but still require valid timing.
Restored automatic age includes time before restart, so restarting does not restart
the 24-hour maximum. Per-event/registered/platform `$session_id` properties and hooks
retain normal Rust precedence over the managed default without replacing manager state.

## Capture and hooks

Capture and screen are synchronous. The SDK snapshots identity, consent and context,
applies registered properties, remembered groups, cached platform context and SDK
defaults, runs hooks, converts the event, then attempts
bounded admission. It performs no filesystem or network work on this path. OS
context is discovered during initialization; embedding applications can provide
`platform_context`, `sdk_name` and `sdk_version` in configuration.

Caller properties win over context/default properties. `$set`, `$set_once`, `$groups`
and `$group_set` objects fill missing inner keys using the existing Rust rules.
Registered properties fill before remembered group defaults, then platform context.
A per-event or registered non-object `$groups` value blocks lower-priority defaults;
explicit typed `Capture.groups` still replace it using the canonical Rust merge.
Typed groups win per group type. Explicit options take precedence over legacy
property options. The SDK supplies `process_person_profile` from the selected profile
mode and identity/profile state (false for fresh anonymous IdentifiedOnly captures).
This option takes precedence even over a caller-supplied legacy
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
returns `Reentrant` when collection is allowed, rather than bypassing privacy hooks. Closing from a hook returns
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
already admitted events. Opt-out, invalid inputs, hook drops, reentrancy, closure and
disabled configuration have distinct outcomes. The byte limit bounds serialized queued data,
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

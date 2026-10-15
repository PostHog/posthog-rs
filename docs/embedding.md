# Native SDK embedding

The optional `embedding` Cargo feature adds durable analytics delivery for host SDKs that own identity, consent, sessions, context, and event preparation. It is off by default. `fs2`, the storage API, prepared-event constructor, delivery controls, and side-effect-free flag exports are enabled only with this feature.

```toml
posthog-rs = { version = "1", default-features = false, features = ["embedding", "tls"] }
```

The feature works with either Rust client. Enable `async-client` when needed. Storage initialization, checkpoint, and resume are blocking operations even on the async client; call them off the host UI thread and use a blocking executor when appropriate.

## Ownership and configuration

Configure `ClientOptionsBuilder::embedding` with `EmbeddingOptions`:

- `store`: an `Arc<dyn EventStore>`. `FileEventStore::open` provides an exclusively locked desktop spool. Choose an isolated directory per project, host, and client instance.
- `sdk_name` and `sdk_version`: transport attribution, sent as `<name>/<version>` in `PostHog-Sdk-Info` and `User-Agent`.
- `enabled`: the host's restored consent state. When false, initialization purges the spool before allowing replay.

Local evaluation and Rust `before_send` hooks are rejected for this configuration. The host must finish enrichment and its hook before submitting an event. Rust does not add runtime properties back to prepared events. V1 still converts reserved processing properties to `options` and session/window properties to their wire fields. `$lib` and `$lib_version` come from the request header and are materialized by PostHog; a per-event hook cannot remove or override transport attribution.

Use `Event::from_prepared` with the original UUID, timestamp, distinct ID, and properties, then `Client::capture_prepared`. Options can be added with `Event::insert_option`. The result distinguishes accepted, disabled, closed, and queue-full admission. **Accepted means bounded memory admission, not durable storage or server receipt.**

The durable contract covers the existing analytics worker. AI and immediate-send APIs do not use this spool or its consent controls. Historical batches are not supported in embedding mode. Do not use those APIs to submit host-owned durable events.

## Persistence and retry

The worker stores prepared events before their first send and restores them without enrichment. Replay preserves their UUID, event timestamp, identity, properties, and options. Storage errors increment the delivery report's `persistence_errors`; an accepted event can still be sent when persistence fails, but is not guaranteed to survive restart.

V1 `ok` and `warning` verdicts retire records as delivered. Terminal rejections retire only the affected records. `retry` verdicts and retryable request failures retain their records and capacity reservations. Exhausting `max_capture_attempts` ends an active sequence, not durable retention. A later explicit flush or timer starts a fresh bounded sequence in the same worker, with a new request ID; scheduled sequences wait at least the flush interval and retry backoff/Retry-After. Explicit flush makes one attempt per held batch and can advance scheduled work immediately.

Queue capacity includes channel entries, buffered events, and retained retry batches. New events are rejected when full. On restart, if the configured capacity is smaller than the recovered spool, the worker keeps the first records returned by the store and removes the overflow. The file store orders records by filename. Corrupt file records are quarantined rather than replayed.

`checkpoint_delivery` waits for preceding worker commands and reports pending events, delivery/drop totals, and storage errors. `flush` is a delivery attempt, not a receipt. Storage is at-least-once best effort: a failed removal or a crash after server persistence can replay an already-delivered UUID.

## Consent and teardown

`pause_and_purge` immediately closes admission and advances the consent generation, then orders a purge of buffered, retrying, and stored analytics. Already-admitted requests may reach the server. `resume_delivery(delivery_generation())` waits for any required purge and rejects stale generations; the host must not persist opt-in or open its own collection gate before this succeeds.

`begin_shutdown` closes admission and establishes the shutdown budget before the host waits for other work. Finish with `shutdown` to join the worker. In-flight request timeouts are capped by that budget; subsequent work uses the remaining deadline. No new event-store `put` begins after expiry. Durable unsent records remain for restart. Filesystem calls and OS scheduling cannot be preempted. Keep the client/store alive until host state writes finish, then drop them to release the spool lock.

## Feature flags

`FeatureFlagEvaluations::export` copies values, decoded payloads, and evaluation metadata without recording accesses or producing exposure events. The host owns cache installation, context generations, and exposure deduplication. Exported payload strings retain the existing evaluator's parsing behavior.

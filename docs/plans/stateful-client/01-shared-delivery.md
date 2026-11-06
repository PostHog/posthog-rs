# PR 1 — Shared delivery and event durability

Status: implemented and locally validated; independent review pending. Predecessor: Rust `v1` at `b244fc38384e44473f4290554f347241a69fbd0f`. Read [the stack contract](README.md) first.

## Outcome and scope

Provide the delivery facilities needed by a stateful client without creating a second analytics transport. Keep event durability independent of identity or client preferences.

Include prepared-event admission, bounded buffering/recovery, durable event storage, retry dispositions, flush/reporting, low-level delivery controls, and worker shutdown. The existing server client remains behaviorally unchanged.

Exclude identity, user consent policy, client-state storage, sessions, flags policy, replay, and the C ABI. Delivery controls are mechanisms; public opt-in/out behavior belongs to PR 4.

## Precedent and contracts

- Android: generic `PostHogQueue`, endpoint codecs/retry policies, disk queue storage, server memory queue.
- JS: stateless core queue/flush/shutdown and platform-selected persistence; Node is memory-backed.
- Specs: event-batcher, retry-queue, http-client, flush, shutdown, persistent-storage (applicable queue portions).
- Existing Rust: analytics transport, v1 verdict handling, and committed embedding work at `de45a689`.

A durable queue that retains retryable events after an active attempt sequence is exhausted is an explicit policy, not a reason to change server retry exhaustion. Document the distinction.

## Implementation tasks

- [x] Trace current server producers, transport lanes, retry pipeline, and shutdown ownership on the selected base.
- [x] Identify the smallest internal prepared-event entry point. Avoid forcing the new client through server enrichment or exposure-producing paths.
- [x] Reuse protocol encoding and request handling. Do not duplicate v1 ingestion or reinterpret reserved properties in two places.
- [x] Define memory-admission results and queue-full/closed outcomes. Accepted does not mean persisted or delivered.
- [x] Adapt/reuse the existing event-store work, with complete-event records and explicit schema/version handling.
- [x] Keep file operations off capture callers. Store records before first delivery when durability is enabled; define persistence-failure fallback explicitly.
- [x] Bound recovery before materializing the full spool. Define count/byte limits and overflow selection when configured capacity shrinks.
- [x] Retire only delivered or terminally rejected records. Preserve UUID, timestamp, identity, properties, and options across retry/restart.
- [x] Provide low-level delivery/shutdown mechanisms only as needed for the agreed consumers; no unused public control API.
- [x] Document flush attempts, reports, retry retention, duplicates after removal failure, and shutdown guarantees.
- [x] Preserve existing server defaults, runtime context, hooks, immediate/AI paths, and feature behavior.

## Decisions before coding

Reuse the existing stateless queue-full policy: reject new events and preserve accepted work. Report disk-write failures through `on_error` and continue bounded memory-only delivery, documenting that unpersisted events can be lost on process exit. Persisted retryable events survive active retry exhaustion and shutdown. Keep existing server behavior unchanged.

Specify record/aggregate bounds, supported filesystem backends, and shutdown deadlines. Keep delivery composition internal until the stateful public client exposes its configuration in PR 2.

Delivery continues for events captured under consent, even after opt-out or restart. PR 4 gates new collection; the shared worker retains its normal persistence, retry, and delivery policy.

## Required tests

- Prepared event reaches the mock unchanged except documented wire conversion; no re-enrichment/hook rerun.
- Mixed v1 verdicts remove only corresponding terminal records.
- Offline admission, graceful close, restart, and successful redelivery preserve event attribution.
- Failure to put/remove/load records produces diagnostics and the approved recovery result.
- Queue bounds include pending/in-flight/retry work; oversized spool recovery stays within approved resources.
- Corrupt/truncated/unknown-version records and competing directory ownership are handled safely.
- Flush during concurrent capture, repeated shutdown, stalled requests, and shutdown during retry backoff.
- Default server capture, immediate/AI paths, blocking/async and TLS combinations remain unchanged.

Use real file-store tests on supported OSes and a controllable store for deterministic failures. Assertions must distinguish graceful restart durability from a crash immediately after memory admission.

## Done gate and handoff

- [x] Shared delivery is exercised by existing Rust callers and focused prepared-event tests, not an unused abstraction.
- [ ] Server regression matrix passes and API changes are explicitly reviewed.
- [x] No user identity or consent semantics have leaked into the generic queue.
- [x] PR 2 can reuse the facility without a new worker implementation.
- [x] Record source/spec references, exact checks, resource-limit decisions, and remaining risks.

### Implementation and validation

Branch: `feat/stateful-delivery`. No predecessor PR; the first draft targets `v1`.

[Delivery documentation](../../stateful-delivery.md) describes admission, storage schema, count/byte bounds, failure reporting, retry retention, and shutdown limits. Queue composition remains internal. The public API snapshot adds only `PostHogError::Storage`, `StorageOperation`, and `StorageFailure` with accessors; the changeset is minor.

Pinned references: SDK specs `dd11a6b8f8c7be3013cd26d97782452ce0f8a76d` (the applicable capability files are unchanged from the plan-review snapshot); Android `bf4d8ef8c86053f34553a6b9f3c5ed4d135057ee`; JS `5663aff913ea5c6ca14dba286829cf7993bea980`. Reject-new overflow preserves existing Rust policy. Durable retry retention is distinct from the server's finite retry budget.

Local checks pass on macOS: full default, stateful, blocking, stateful/blocking, MCP/Tower, workspace, compatibility-crate, and TLS-provider test combinations; Clippy; public API snapshot; Cargo package verification; and Rust 1.78 library checks with and without stateful, for both client flavors. Public blocking and async consumers also pass against the maintained SDK mock server, covering partial retries, hook counts, unchanged retry payloads, gzip, immediate capture, AI routing, and shutdown. No live ingestion was used.

CI adds Windows/macOS storage-and-transport coverage and stateful Linux/MSRV configurations. Those remote jobs have not run yet. Initialization/default configuration and public stateful entry points belong to PR 2; record and prepared-memory limits must be coherent. Filesystem calls cannot be forcibly interrupted, and process-crash persistence is not power-loss durability.

Independent SDK review and its second pass remain required before committing and submitting this draft.

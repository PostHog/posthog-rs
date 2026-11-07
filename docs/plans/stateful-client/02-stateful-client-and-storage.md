# PR 2 — Stateful client, memory-first storage, and event pipeline

Status: public foundation implemented and locally validated on `feat/stateful-client`, based on the signed delivery foundation `21376942697749e3f89dfc39a0ec508480442342` ([draft PR 334](https://github.com/PostHog/posthog-rs/pull/334)). Predecessor: [PR 1](01-shared-delivery.md). Read [the stack contract](README.md).

## Outcome and scope

A standalone Rust consumer can initialize a stateful client, capture under a generated anonymous identity, read that identity synchronously, restart with persisted identity, flush, and shut down.

This PR establishes the reusable client-state storage and synchronization model for every later domain. It includes anonymous identity bootstrap, explicit screen capture, SDK attribution, platform context input, event assembly, native Rust before-send hooks, and the shared-delivery integration.

Exclude identify/person mutation APIs, consent transitions, registered/groups APIs, sessions, lifecycle analytics, flags, C ABI, and Unreal. Do not expose placeholder methods for later phases.

## Precedent and contracts

- JS: `PostHogCore` over `PostHogCoreStateless`; abstract persisted-property access; React Native's synchronous cache and deferred storage.
- Android: shared stateful `PostHog`, injected preferences, platform `SharedPreferences.apply()`.
- Specs: setup, bootstrap, capture, screen, get-anonymous-id, get-distinct-id, device-id-generator, before-send-hook, persistent-storage, flush, shutdown.
- Property precedence, merging, and option/session-field lifting follow the existing Rust stateless implementation. Stateful events retain capture-time `$lib` and `$lib_version` in their properties and apply the capture spec's null custom-property cleanup without changing existing server serialization.

The Rust file backend is a platform implementation of this storage pattern, not a requirement that a consumer implement filesystem operations.

## State and storage contract

- Core-visible state is authoritative in memory after initialization. Getters do not reread disk.
- A mutation validates, updates memory, and schedules persistence before returning; it never waits for a write.
- The storage boundary must make nonblocking scheduling explicit. A synchronous setter that directly invokes arbitrary filesystem code does not satisfy the contract.
- Persist immutable snapshots/revisions in order. Serialize writes or otherwise prevent an older completion from overwriting newer state.
- Coalescing may skip intermediate snapshots but must persist the newest state eventually; bound delay under continuous traffic rather than resetting a debounce timer forever.
- Keep at most bounded pending/in-flight work. Separate optional cache eviction from mandatory state bounds.
- Define a drain through a captured revision, so shutdown/tests can await preceding changes without chasing future writes forever.
- Report write errors; keep live memory state. Retain/retry dirty state according to a bounded documented policy rather than silently clearing it after failure.
- File backend uses versioned bounded records and safe replacement. Do not claim power-loss guarantees beyond the actual OS operations.
- Namespace ownership covers both state and event stores; acquire ownership before restore. Do not independently lock the same namespace twice and deadlock construction.

A slow backend must not hold the client-state lock while doing I/O. Later domains must use this shared machinery, not each introduce a worker/cache/write loop.

## Implementation tasks

- [x] Define public type/config names, feature exposure, server compatibility, and one blocking/async semantic model.
- [x] Define ready/closing/closed states, pre-ready behavior, nonblocking initialization, and initialization failure results.
- [x] Implement memory-first state storage with in-memory, delayed/failing test, and desktop file backends (private foundation; public integration remains below).
- [x] Define schema version, instance namespace inputs, memory-only mode, corruption/future-version behavior, and default persistence selection.
- [x] Generate/restore anonymous identity and expose local ID getters. Support anonymous bootstrap on a fresh scope without replacing persisted identity; PR 3 adds identified bootstrap and reconciliation.
- [x] Define current distinct ID and anonymous ID as distinct persisted concepts. Fresh anonymous identities use UUIDv7; `$device_id` derives from the current anonymous ID rather than an additional identifier. PR 3 owns identified transitions and reset lifetimes.
- [x] Assemble event UUID/time, identity, caller properties, cached platform context, SDK attribution, and initial anonymous person-processing semantics.
- [x] Invoke hooks outside state locks; validate transformed events; use PR 1 for final admission without adding server context.
- [x] Implement `screen(name, properties)` as `$screen` capture with `$screen_name` from the explicit name argument. Use the common pipeline; leave automatic screen observation and navigation integration to consumers.
- [x] Support deterministic clocks/IDs where needed for tests, without a general plugin framework.
- [x] Implement flush/shutdown coordination for state writes and event delivery, with no self-join or unsafe detached callbacks.
- [x] Add an engine-free public Rust example and update API snapshot/documentation.

## Agreed foundation design

- Async and explicitly blocking initialization return `Client` directly after local initialization completes. Invalid project-token configuration reports through `on_error` and sets the same client's disabled flag; API methods check that flag and safely no-op. Essential local delivery initialization failure also produces a disabled client. Initialization does not wait for network reachability. Ordinary capture and getters are synchronous. Runtime ownership stays with the caller; delivery uses the existing std-thread engine.
- Disk persistence is automatic by default, with configurable directory and memory-only modes. Use the OS local application-data directory (`dirs::data_local_dir`) plus `posthog/<namespace>`: `$XDG_DATA_HOME` or `~/.local/share` on Linux, `~/Library/Application Support` on macOS, and the LocalAppData known folder on Windows. An unavailable/invalid OS location produces a storage diagnostic and memory fallback, not a guessed working-directory location.
- A length-delimited hash of normalized ingestion host, project token, and instance name isolates each namespace; raw credentials and instance strings are not path components. State and event storage share the same exclusive lease. Use an instance name or directory override when applications should use separate state for the same project.
- Storage errors report through `on_error` and do not fail initialization. Ownership failure disables both stores rather than bypassing the lock. A failed state restore leaves the original file untouched and disables state writes for that instance; an otherwise usable event store can continue. An event-store open failure does not discard successfully restored identity or the state writer.
- One dedicated state-persistence thread serves all later domains, separate from HTTP delivery. Keep authoritative state in memory, at most one in-flight and the latest pending snapshot, bounded coalescing, and ordered writes/retries. Shutdown drains a captured revision rather than chasing later updates.
- Version-1 state is a bounded JSON record containing anonymous and current distinct identity. Validate the same decoder before replacing a file. Unrecognized fields/versions, invalid mandatory identity, and oversized records cannot produce a writable state store. Later schema changes must define compatible defaults or migration; unknown state must not be overwritten on downgrade.
- Replace `state.json` via a private same-directory temporary, syncing the file and, on Unix, its directory. An abandoned temporary is not committed state. This is not a transaction across state/event stores or a universal power-loss guarantee.
- Native Rust hooks run outside state locks, fail closed on panic/drop, and cannot recursively capture into the same client or deadlock by joining their own worker. Retain transformed capture-time SDK metadata through persistence/retry; no attribution rewrite at restart. End-to-end ingestion attribution depends on PostHog's rollback of header-authoritative SDK-property stamping.
- Public defaults: flush at 20 events or 10 seconds; batches up to 100; resident queue 1,000 events / 8 MiB; event store 10,000 records / 64 MiB; persisted event record and mandatory state each at most 1 MiB; gzip; GeoIP enabled; no server marker. State coalescing is bounded to 250 ms while the worker is available, retry backoff to 30 seconds. Request and shutdown-drain budgets remain 30 seconds. Validate positive/coherent bounds, including event record size <= resident byte budget.
- JS core/React Native provides the 20/100/1,000/10-second precedent. Android, Apple (except tvOS) and Unity use 20/50/1,000/30 seconds. Byte bounds are deliberate Rust limits, not a cross-SDK consensus; serialized-byte bounds do not equal allocator-memory bounds.

Do not hold locks through user hooks or rewrite an event's identity after its snapshot. PR 4 checks consent at the capture snapshot before hooks; subsequent opt-out does not invalidate that capture.

## Required tests

- Anonymous identity survives drain/restart; separate projects/hosts/instances do not share state.
- Absent bootstrap has no effect; anonymous bootstrap seeds a fresh scope and cannot overwrite persisted identity. Concurrent first access yields one identity.
- Screen emits `$screen`, the explicit name wins over a conflicting `$screen_name`, and hooks/enrichment/delivery match ordinary capture.
- Like-for-like property collisions, defaults, hooks, and wire conversion preserve Rust stateless semantics.
- A backend blocked by a test barrier cannot delay capture or local getters/mutations; verify file I/O thread identity.
- Coalesced updates persist the latest snapshot; delayed old writes cannot overwrite new state.
- Failed writes preserve memory and dirty status; later persistence can recover.
- Missing/corrupt/truncated/oversized/future-version state and startup store failure have defined outcomes.
- Hook mutation/drop/panic and reentrant capture/close do not leak unfiltered events or deadlock.
- Events carry correct SDK attribution and omit server enrichment.
- Initialization races, repeated shutdown, pending writes, worker failure, and teardown during callbacks.
- Public example works in each declared client integration mode; server tests still pass.

## Implementation progress

The private storage and state-writer foundations live in `src/client/stateful/`. State updates validate a candidate before replacing the authoritative in-memory snapshot and incrementing its revision. The single writer retains one in-flight snapshot and the latest state, so continuous updates do not grow a queue or indefinitely reset the coalescing deadline. Blocking and async revision waits use the same progress state; async waits use notifications without owning a runtime or timer.

Failed writes retain dirty state and retry the latest snapshot with exponential backoff from 100 ms to 30 seconds. A checkpoint expedites coalescing without bypassing a retry cooldown. Waiting through a captured revision can report a completed persistence attempt, write failure, intentional memory-only operation, or unavailable persistence; it does not chase future changes. Shutdown closes mutations, drains within one fixed budget, and joins externally. In-flight filesystem operations/callbacks can exceed that budget; close from the writer's own callback is deferred rather than self-joining.

The shared transport now has a private stateful startup path that acknowledges local HTTP initialization before returning its handle. It does not wait for recovery reads or network delivery; recovery can temporarily reserve admission capacity after readiness. Both persistent and memory-only modes use bounded prepared-event admission. Failed startup joins the worker and releases its storage lease before returning to the initializer, which can report the failure and disable the client.

The shared event-to-wire builder has a stateful conversion path that retains producer-prepared, capture-time `$lib` and `$lib_version` properties. It preserves the existing Rust option/group/session-field conversion rules and does not fill missing or hook-removed SDK properties. The server conversion continues to remove SDK properties in favor of its existing header behavior. Real-file and shared-worker tests cover complete record fidelity, mixed SDK versions in one recovered batch, partial acknowledgment, and restart of a retained retry without rerunning hooks. The public producer uses this boundary after enrichment, native hooks and custom-property normalization.

`posthog::stateful::{Client, Config, Persistence}` exposes direct-return initialization in blocking and async modes, synchronous identity getters, disabled-state inspection, and coordinated blocking/async flush/shutdown reports. Configuration errors report sanitized `PostHogError::Initialization` diagnostics before opening storage. Disabled getters return empty strings and drains are no-ops. Storage errors keep the client enabled. Async initialization uses a std initialization thread and a runtime-independent completion. The thread retains ownership until the receiving future claims the client; only a readiness notification is buffered for the future. Cancellation signals the initializer to tear down an unclaimed client, including after readiness was signalled.

Fresh anonymous UUIDv7 identities and anonymous bootstrap persist through the shared writer; restored identity wins over bootstrap. Blocking drains distinguish state persistence from local delivery progress rather than acknowledging ingestion. Both workers are requested to close before either is joined, and calls from either worker defer waiting. Destruction on either worker also relinquishes the state-writer join handle; each worker retains its own state, callbacks, and backend until it exits. Public `flush().await` and `shutdown(timeout).await` use the same workers and outcomes as blocking drains. Both shutdown methods take `Option<Duration>`: `None` selects the configured default and `Some` overrides it, clamped to one day. The first shutdown request selects one absolute scheduling deadline for both workers; repeated calls and Drop cannot restart the budget. Async completion waits use runtime-independent channels, including a shared delivery-worker exit notification for concurrent/cancelled shutdown waiters. Final joins remain synchronous and can exceed the budget or block polling. This best-effort limitation differs from the strict whole-teardown timeout requirement in the shutdown specification. Cancellation of a pending drain does not cancel requested work or reopen admission; dropping the client still performs blocking cleanup.

`Capture` supplies event properties/options/groups and optional time/UUID without requiring identity. `Client::capture` snapshots identity once, applies cached platform and SDK defaults with the shared Rust merging rules, runs native hooks synchronously outside state locks, validates the transformed event and submits a prepared snapshot to the existing bounded delivery engine. `screen` delegates to this pipeline. Custom null object members are removed after hooks; array slots and reserved person/group mutation payloads, wire options and envelope semantics are preserved. A thread-local guard rejects recursive same-client capture, defers drains from hooks, and safely fails closed if the thread-local context has already been destroyed. Hook-triggered closure prevents outer admission.

Public integration tests cover both feature modes; controlled state I/O proves capture/screen/getters and hook-triggered close do not wait on persistence. The maintained SDK mock smoke exercises gzip, captured identity/attribution, partial retry across process restart, hook/null fidelity, unsupported-state memory fallback and the repository example in both modes. A fresh consumer dependency resolution also exercises SHA-1 0.11; namespace formatting uses bytes and retains the same zero-padded version-1 names verified with the locked SHA-1 0.10 dependency.

## Local validation

The full local matrix on macOS with Rust 1.96.1 passed: 532 default tests, 659 stateful, 440 blocking, 600 stateful/blocking with error tracking and TLS, 758 stateful/MCP/Tower, and 667 workspace tests. Each root suite retains one existing ignored test. Locked Rust 1.78 checks pass for default/stateful and async/blocking configurations. API snapshot, compatibility crate, all-target Clippy, package verification, and public server/stateful mock smoke pass. Stateful provider-free TLS initialization is exercised through both constructors; the compatibility crate checks canonical type identity and disabled-client public entry points.

The CI matrix runs the stateful client/storage/transport and public initialization/capture/drain tests on macOS and Windows, including the blocking configuration; broader feature suites run on Linux. These are configured remote gates, not results of the local macOS run. Async Rustdoc is warning-free; blocking Rustdoc retains the pre-existing unresolved `AsyncFlagPoller` link at `src/local_evaluation.rs:305:7`.

Final joins can block async polling and exceed the shutdown budget. State/event persistence is not a cross-store transaction or universal power-loss guarantee. Raw-wire and restart attribution are validated locally; production ingestion attribution still depends on backend header-stamping behavior. Independent full-domain review and remote platform results remain separate from these local test results.

## Done gate and handoff

- [x] Useful anonymous capture is available through public Rust APIs.
- [x] Memory visibility and asynchronous persistence are independently demonstrated.
- [x] No ordinary capture/state operation waits for disk/network I/O.
- [x] Schema and ordering permit later domains to extend the same state store; additions must define compatible defaults or migration and use the shared writer.
- [x] Exact feature/toolchain/filesystem evidence and unresolved limits are recorded.

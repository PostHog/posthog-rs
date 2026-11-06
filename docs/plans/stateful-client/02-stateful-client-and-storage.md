# PR 2 — Stateful client, memory-first storage, and event pipeline

Status: planned. Predecessor: [PR 1](01-shared-delivery.md). Read [the stack contract](README.md).

## Outcome and scope

A standalone Rust consumer can initialize a stateful client, capture under a generated anonymous identity, read that identity synchronously, restart with persisted identity, flush, and shut down.

This PR establishes the reusable client-state storage and synchronization model for every later domain. It includes anonymous identity bootstrap, explicit screen capture, SDK attribution, platform context input, event assembly, native Rust before-send hooks, and the shared-delivery integration.

Exclude identify/person mutation APIs, consent transitions, registered/groups APIs, sessions, lifecycle analytics, flags, C ABI, and Unreal. Do not expose placeholder methods for later phases.

## Precedent and contracts

- JS: `PostHogCore` over `PostHogCoreStateless`; abstract persisted-property access; React Native's synchronous cache and deferred storage.
- Android: shared stateful `PostHog`, injected preferences, platform `SharedPreferences.apply()`.
- Specs: setup, bootstrap, capture, screen, get-anonymous-id, get-distinct-id, device-id-generator, before-send-hook, persistent-storage, flush, shutdown.
- Property precedence, merging, and wire conversion follow the existing Rust stateless implementation; stateful enrichment must preserve those semantics.

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

- [ ] Agree public type/config names, feature exposure, server compatibility, and one blocking/async semantic model.
- [ ] Define ready/closing/closed states, pre-ready behavior, nonblocking initialization, and initialization failure results.
- [ ] Implement memory-first state storage with in-memory, delayed/failing test, and desktop file backends.
- [ ] Define schema version, instance namespace inputs, memory-only mode, corruption/future-version behavior, and default persistence selection.
- [ ] Generate/restore anonymous identity and expose local ID getters. Support anonymous bootstrap on a fresh scope without replacing persisted identity; PR 3 adds identified bootstrap and reconciliation.
- [ ] Define current distinct ID, anonymous ID, and any optional separate device ID as distinct concepts, with generation, persistence, and reset lifetimes. Use UUIDv7-style anonymous generation per spec; a separate long-lived device ID requires a demonstrated consumer, not automatic copying of React Native's platform-specific layer.
- [ ] Assemble event UUID/time, identity, caller properties, sampled context, SDK attribution, and initial anonymous person-processing semantics.
- [ ] Invoke hooks outside locks; validate transformed events; use PR 1 for final admission without adding server context.
- [ ] Implement `screen(name, properties)` as `$screen` capture with `$screen_name` from the explicit name argument. Use the common pipeline; leave automatic screen observation and navigation integration to consumers.
- [ ] Support deterministic clocks/IDs where needed for tests, without a general plugin framework.
- [ ] Implement flush/shutdown coordination for state writes and event delivery, with no self-join or unsafe detached callbacks.
- [ ] Add an engine-free public Rust example and update API snapshot/documentation.

## Decisions before coding

Choose state schema/backend interface, default persistence and bounds, initialization failure behavior, SDK attribution API, and hook error/reentrancy behavior. Prefer native Rust hooks first; preserve the ability to bridge them later without defining a speculative public C API.

Decide how capture snapshots interact with concurrent state changes. Do not hold locks through user hooks or rewrite an event's identity after its snapshot. PR 4 checks consent at the capture snapshot before hooks; subsequent opt-out does not invalidate that capture.

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

## Done gate and handoff

- [ ] Useful anonymous capture is available through public Rust APIs.
- [ ] Memory visibility and asynchronous persistence are independently demonstrated.
- [ ] No ordinary capture/state operation waits for disk/network I/O.
- [ ] Schema and ordering permit later domains to extend the same state store.
- [ ] Exact feature/toolchain/filesystem evidence and unresolved limits are recorded.

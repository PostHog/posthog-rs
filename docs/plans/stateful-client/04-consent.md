# PR 4 — Consent

Status: implemented and routine-reviewed; full local release validation passed. Full-domain review and publication gates pending. Predecessor: [PR 3](03-identity-and-person-profiles.md). Uses delivery from PR 1 and storage from PR 2.

## Outcome and scope

Add persisted opt-in/out and synchronous collection gating to the stateful Rust client. A call after opt-out returns observes opted-out state immediately; a call after opt-in returns observes opted-in state immediately. Neither operation waits for file I/O.

This is the agreed calling contract, not a durable-acknowledgement API. Follow [the stack storage contract](README.md).

## Precedent and contracts

- Android updates its in-memory configuration under a lock, then writes through preferences; the standard backend uses `apply()` for deferred disk persistence.
- JS core writes its persisted-property interface; React Native uses a memory cache and deferred storage.
- Node's enable/disable uses memory storage only and does not establish restart persistence.
- Specs: opt-in, is-opt-out, consent-gating, persistent-storage, reset.
- Consent is prospective: previously consented captures and requests retain their normal completion and delivery policy.

## Required contract

- `Config::opt_out` is the startup default, false by default. An explicit persisted choice overrides it; absent or legacy consent uses this default. Resolve consent before ready/admission so no startup capture escapes persisted opt-out.
- Expose synchronous `opt_in()` / `opt_out()` returning `()`, a memory-only `is_opt_out()` getter, and `CaptureOutcome::OptedOut`. Disabled/closed clients are effectively opted out; opting in cannot enable them.
- Opt-out updates memory and closes collection in the same ordered transition; schedule persistence afterward without disk waits.
- Opt-in changes memory and allows fresh collection without waiting for persistence.
- Repeated calls are safe and linearizable with capture.
- Analytics capture from identify/person/group and eventual automatic events/exposures uses the same gate. Do not gate the entire identify operation: local identity and targeting state still update while opted out, and successful-capture deduplication remains unchanged for blocked events.
- Check consent atomically with the capture snapshot before invoking hooks. A capture permitted at that point may finish hooks, queue admission, persistence, and delivery after opt-out. A separate capture initiated from a hook checks consent independently.
- Reset clears the explicit consent choice and returns to the configured startup default, atomically with the fresh identity. With the ordinary enabled default, `opt_out(); reset();` permits new collection; with `Config::opt_out = true`, `opt_in(); reset();` blocks collection. Reset does not reapply identity bootstrap.
- Failed persistence leaves the current in-memory choice intact and reports the error. An immediate process crash can restore an older persisted choice.
- Queued events and requests started under consent may complete and retry normally after opt-out. A response must not change the current consent state; any new analytics capture triggered by completion checks current consent.

## Previously consented work

Opt-out prevents new collection; it does not cancel in-progress captures or requests, purge queued events, or block their persistence, retry, or delivery. Restored queue records captured under consent remain eligible for delivery even when the restored consent state is opted out.

Normal hook filtering, queue bounds, retry limits, response-context validation, and shutdown still apply. Permission to finish existing work does not authorize new collection after withdrawal.

Feature-flag loading remains allowed while opted out, including reloads caused by identify. Its analytics exposure events remain gated. Other remote-config capabilities must document their own initiation rules; a later opt-out alone does not require cancelling a request or discarding its result.

## Implementation tasks

- [x] Record defaults, the prospective consent boundary, request-initiation policy, and reset interaction.
- [x] Add consent to the shared in-memory state and asynchronous persistence schema.
- [x] Implement opt-in/out/getter and the synchronized capture-time consent check.
- [x] Integrate existing identity/person APIs and hooks with the gate.
- [x] Expose safe diagnostics for persistence failures without logging user state.
- [x] Document the distinction between immediate collection changes and crash durability.

## Required tests

- Capture immediately after opt-out is rejected; immediately after opt-in it is admitted while storage is deliberately blocked.
- Getters report the new choice synchronously; write completion/failure cannot revert it.
- Stored opt-out blocks early new capture while previously consented queue records remain deliverable.
- A capture permitted before a blocked hook can complete after opt-out, with or without a subsequent opt-in; a new capture from that hook while opted out is rejected.
- Concurrent opt-in/out cannot allow stale completion to override newer consent.
- Repeated toggles and coalesced state writes persist the latest revision after drain.
- Queued/in-flight/retrying work captured or started under consent continues normally after opt-out and queue recovery.
- A request completing after opt-out may install a context-valid result, but cannot re-enable collection or emit new analytics while opted out.
- Identify and identified-bootstrap reconciliation while opted out update local identity without analytics emission or successful-capture deduplication; opt-in retains that identity. PR 8 adds targeting/reload assertions.
- Crash-point/restart tests distinguish durable state from pending memory updates.
- Reset, shutdown, and delayed persistence interact without deadlocks or false durability claims.

## Implementation and validation

The existing snapshot carries an optional explicit consent choice; immutable producer configuration
supplies the default. Version 3 stores the explicit choice, including null after reset. Versions 1
and 2 migrate without an explicit choice, retaining their identity/profile semantics. Capacity
validation reserves the longest consent representation so toggling never needs more state space.
Unsupported or oversized migration preserves the old file and uses configured-default memory fallback.

Public tests cover defaults/restored overrides, reset/restart, identity and bootstrap while out,
helper guards, delayed/nested hooks, concurrency, write failure, bounded migration, and delivery of
persisted pre-opt-out events. A gated-file test separately proves synchronous consent/capture while
I/O is blocked and that old write completion cannot revert memory. Compatibility-shim tests cover
the consent API, and macOS/Windows CI selects the public consent suite in both feature modes.
The local feature/MSRV/API/package/documentation matrix and maintained-mock smoke pass. The
smoke exercises public async and blocking entry points, durable pre-opt-out retry/restart,
persisted choices overriding defaults, identity updates without linking replay, and both reset
defaults. Local execution is macOS; remote platform execution and full-domain reviews remain
separate gates.

PR 8 must add actual request-completion/targeting tests when the stateful flag loader exists;
there is no stateful flag request subsystem in this domain. Existing delivery keeps its normal
retry/recovery policy and never reads current consent. Later event producers must use the same
snapshot gate rather than adding a second delivery gate.

## Done gate and handoff

- [x] Immediate memory gating and asynchronous storage are proven independently.
- [x] Previously consented work can complete while new collection is blocked.
- [x] Consent is the single policy source for later domains.
- [x] Tests and documentation state crash/in-flight limitations precisely.
- [ ] Full-domain review, signed draft publication and remote CI complete.

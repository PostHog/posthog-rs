# PR 4 — Consent

Status: planned. Predecessor: [PR 3](03-identity-and-person-profiles.md). Uses delivery from PR 1 and storage from PR 2.

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

- Consent defaults and restored state are resolved before ready/admission; no startup capture escapes persisted opt-out.
- Opt-out updates memory and closes collection in the same ordered transition; schedule persistence afterward without disk waits.
- Opt-in changes memory and allows fresh collection without waiting for persistence.
- Repeated calls are safe and linearizable with capture.
- Analytics capture from identify/person/group and eventual automatic events/exposures uses the same gate. Do not gate the entire identify operation: local identity and targeting state still update while opted out, and successful-capture deduplication remains unchanged for blocked events.
- Check consent atomically with the capture snapshot before invoking hooks. A capture permitted at that point may finish hooks, queue admission, persistence, and delivery after opt-out. A separate capture initiated from a hook checks consent independently.
- Reset's treatment of consent follows the selected canonical contract; never silently preserve or clear it based on an unrelated implementation convenience.
- Failed persistence leaves the current in-memory choice intact and reports the error. An immediate process crash can restore an older persisted choice.
- Queued events and requests started under consent may complete and retry normally after opt-out. A response must not change the current consent state; any new analytics capture triggered by completion checks current consent.

## Previously consented work

Opt-out prevents new collection; it does not cancel in-progress captures or requests, purge queued events, or block their persistence, retry, or delivery. Restored queue records captured under consent remain eligible for delivery even when the restored consent state is opted out.

Normal hook filtering, queue bounds, retry limits, response-context validation, and shutdown still apply. Permission to finish existing work does not authorize new collection after withdrawal.

Feature-flag loading remains allowed while opted out, including reloads caused by identify. Its analytics exposure events remain gated. Other remote-config capabilities must document their own initiation rules; a later opt-out alone does not require cancelling a request or discarding its result.

## Implementation tasks

- [ ] Record defaults, the prospective consent boundary, request-initiation policy, and reset interaction.
- [ ] Add consent to the shared in-memory state and asynchronous persistence schema.
- [ ] Implement opt-in/out/getter and the synchronized capture-time consent check.
- [ ] Integrate existing identity/person APIs and hooks with the gate.
- [ ] Expose safe diagnostics for persistence failures without logging user state.
- [ ] Document the distinction between immediate collection changes and crash durability.

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

## Done gate and handoff

- [ ] Immediate memory gating and asynchronous storage are proven independently.
- [ ] Previously consented work can complete while new collection is blocked.
- [ ] Consent is the single policy source for later domains.
- [ ] Tests and documentation state crash/in-flight limitations precisely.

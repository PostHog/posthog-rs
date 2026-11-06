# PR 3 — Identity and person profiles

Status: planned. Predecessor: [PR 2](02-stateful-client-and-storage.md). Follow [shared contracts](README.md).

## Outcome and scope

Add stateful identify, alias, person-property updates, profile-processing modes, and reset. Identity transitions are synchronous memory operations with asynchronous persistence.

Include anonymous/identified status, distinct-ID changes, linking events, identified bootstrap/reconciliation, per-event person-processing options, and property-update deduplication. Reset clears the user state present at this stack stage and retains queued events.

Later PRs extend reset to their own properties, consent, sessions, flags, and lifecycle bookkeeping. Do not add empty caches/managers in anticipation of them.

## Precedent and contracts

- JS `PostHogCore`: identify, alias, reset, person profile modes and set-person-properties.
- Android shared `PostHog`: identity transitions, identified flags, person processing, and property deduplication.
- Specs: bootstrap, identify, alias, reset, get-anonymous-id, get-distinct-id, create-person-profile, set-person-properties, capture.
- Rust explicit-ID server APIs remain stateless and keep their existing behavior.

Check peer/spec differences rather than treating Unreal's refusal to change an identified ID as authoritative.

## Behavioral requirements

- Fresh state is anonymous unless an identified bootstrap applies; identify determines whether linking and/or property events are needed.
- Identified bootstrap seeds the distinct ID and identified status without adopting the person's ID as the anonymous/device ID. Reconcile an existing anonymous user through identify; preserve a different already-identified user and warn rather than switching silently. A matching anonymous ID can be marked identified without a redundant `$identify`.
- Opt-out is not an identify guard: local identity and, once PR 8 exists, person targeting state still update. PR 4 gates only its analytics capture; a blocked event must not update successful-capture deduplication. Bootstrap reconciliation follows the same rule.
- Subsequent calls see new identity immediately; storage completion is not an identity-transition barrier.
- Construct identity/person events through the same hooks, consent-ready admission boundary, protocol conversion, and delivery path as ordinary capture.
- Define Never / IdentifiedOnly / Always behavior and defaults against the spec. Decide explicitly whether a separate create-person-profile API is needed for capability coverage.
- Alias links identities without silently changing the current identity unless the canonical API says otherwise.
- Reset emits no reset event, creates the approved fresh anonymous context, and does not purge/rewrite queued events.
- Decide what an identify/property mutation returns when its event is dropped by a hook or rejected by queue capacity. State change and delivered identify event are not an atomic transaction.
- Deduplication must not hide a needed retry after rejected admission. Bound any stored dedup metadata.
- Preserve installation-scoped metadata when later domains introduce it; do not implement reset as indiscriminate directory deletion.

## Implementation tasks

- [ ] Write a transition table: anonymous/identified × same/different/missing ID × property changes × profile mode.
- [ ] Implement public methods and state changes using PR 2's store.
- [ ] Extend setup bootstrap with identified status and persisted-identity reconciliation before ready. Reset must not replay the original setup bootstrap.
- [ ] Record identify-to-flag context transitions for PR 8 without adding a placeholder flag cache in this PR.
- [ ] Reuse event building for identify/alias/person updates and processing options.
- [ ] Introduce a clear internal reset operation over existing user-scoped state.
- [ ] Define snapshot/generation handling for concurrent identify/reset/capture and delayed hooks.
- [ ] Document defaults, invalid inputs, event admission versus state change, and restart behavior.
- [ ] Extend the public example and native wire acceptance tests.

## Required tests

- Anonymous restart and identify linking preserve correct anonymous/current IDs and event fields.
- Duplicate identify, same-ID property updates, switching identified IDs, and invalid IDs follow the transition table.
- Fresh identified bootstrap uses a separate anonymous/device identity; persisted anonymous reconciliation, matching-ID upgrade, and preservation of a different identified user follow the bootstrap scenarios.
- Plain reset creates fresh identity rather than reapplying setup bootstrap; PR 4 extends reconciliation tests to opted-out state.
- Person modes gate operations and wire processing options correctly, including anonymous updates.
- Set/set-once and alias have correct wire payloads.
- Hook drop/mutation, full queue, and write failure have approved state and dedup results.
- Reset changes subsequent identity, emits no reset event, and preserves original queued-event attribution.
- Concurrent captures see coherent old/new snapshots, not mixed identity and person state.
- Slow storage does not delay identify/reset/getters; restart after drain uses the latest state.
- Server identify/alias/capture behavior does not acquire ambient identity.

## Done gate and handoff

- [ ] Canonical identity/person scenarios pass through public Rust entry points.
- [ ] All edge-case policy choices are documented with peer/spec evidence.
- [ ] Reset has one owner that later domains can extend atomically.
- [ ] Record exact checks, defaults, and remaining domain interactions for PR 4.

# PR 3 — Identity and person profiles

Status: implemented; validation/review/release gates pending. Predecessor: [PR 2](02-stateful-client-and-storage.md). Follow [shared contracts](README.md).

## Outcome and scope

Add stateful identify, alias, person-property updates, profile-processing modes, and reset. Identity transitions are synchronous memory operations with asynchronous persistence.

Include anonymous/identified status, distinct-ID changes, linking events, identified bootstrap/reconciliation, per-event person-processing options, and property-update deduplication. Reset clears the user state present at this stack stage and retains queued events.

Later PRs extend reset to their own properties, consent, sessions, flags, and lifecycle bookkeeping. Do not add empty caches/managers in anticipation of them.

## Precedent and contracts

- Browser `PostHog`: profile modes and admission-aware property deduplication. Browser's already-identified switching differs from Android and is not used here.
- Shared JS `PostHogCore` is a separate identity implementation; Node uses stateless explicit-ID operations.
- Android shared `PostHog`: identity transitions, identified flags, person processing, and property deduplication.
- Specs: bootstrap, identify, alias, reset, get-anonymous-id, get-distinct-id, create-person-profile, set-person-properties, capture.
- Rust explicit-ID server APIs remain stateless and keep their existing behavior.

Use Android's reset-before-switch rule. Identified A → B leaves A unchanged and emits nothing. Browser changes the local ID without a second `$identify`; Node has no ambient identity to switch.

## Behavioral requirements

- Fresh state is anonymous unless an identified bootstrap applies; identify determines whether linking and/or property events are needed.
- Identified bootstrap seeds the distinct ID and identified status without adopting the person's ID as the anonymous/device ID. Reconcile an existing anonymous user through identify; preserve a different already-identified user and warn rather than switching silently. A matching anonymous ID can be marked identified without a redundant `$identify`.
- Opt-out is not an identify guard: local identity and, once PR 8 exists, person targeting state still update. PR 4 gates only its analytics capture; a blocked event must not update successful-capture deduplication. Bootstrap reconciliation follows the same rule.
- Subsequent calls see new identity immediately; storage completion is not an identity-transition barrier.
- Construct identity/person events through the same hooks, consent-ready admission boundary, protocol conversion, and delivery path as ordinary capture.
- Default to IdentifiedOnly; Never ignores identity/profile operations, Always processes captures by default. Explicit capture options and hooks retain Rust precedence. Expose create-person-profile independently of ordinary empty property updates.
- Alias links identities without silently changing the current identity unless the canonical API says otherwise.
- Reset emits no reset event, creates the approved fresh anonymous context, and does not purge/rewrite queued events.
- Mutation APIs return `()`. Local transitions remain applied after hook drop or capacity refusal; no rollback or automatic linking replay. State change and delivered identify event are not an atomic transaction.
- Deduplication must not hide a needed retry after rejected admission. Bound any stored dedup metadata.
- Preserve installation-scoped metadata when later domains introduce it; do not implement reset as indiscriminate directory deletion.

## Transition table

Never ignores identify calls. Disabled/closed clients, reentrant emitting calls, blank IDs and state-size failures do not mutate identity.

| Prior state | ID | Properties | Local effect | Event |
| --- | --- | --- | --- | --- |
| Anonymous | Different | Any | Identify; retain anonymous/device ID | `$identify` with previous anonymous distinct ID |
| Anonymous | Same | Any, including empty | Mark identified | One `$set` |
| Identified | Same | Changed, nonempty | Identity unchanged | `$set` |
| Identified | Same | Empty or accepted duplicate | Unchanged | None |
| Identified | Different | Any | Unchanged; reset required | None |

Bootstrap matching an existing anonymous ID marks identified without an event. Never gates restored-state reconciliation, while a fresh identified bootstrap still seeds identity with a separate device ID. An already-identified bootstrap mismatch preserves the existing identity.

## Implementation tasks

- [x] Write the transition table above.
- [x] Implement public methods and state changes using PR 2's store.
- [x] Extend setup bootstrap with identified status and persisted-identity reconciliation before ready. Reset does not replay setup bootstrap.
- [x] Record identify-to-flag context transitions below without a placeholder cache.
- [x] Reuse event building for identify/alias/person updates and processing options.
- [x] Introduce one reset operation over existing user-scoped state.
- [x] Capture immutable operation snapshots; generation checks prevent late dedup updates crossing identify/reset.
- [x] Document defaults, invalid inputs, admission versus state change and restart behavior.
- [x] Extend the public example and native wire tests.

## State and later-domain handoff

State version 2 adds identified/profile flags; version 1 restores as anonymous and migrates on a later write. Unknown versions remain preserved and unavailable for writes. Process-local generation and fixed-size property fingerprints share the state lock but are not persisted and do not create persistence revisions. The namespace and event formats are unchanged.

PR 4 adds prospective consent checks at capture admission, retaining identify's local updates. Alias and profile-operation consent guards must follow their capability specs. PR 8 adds real person targeting context: new-ID identify carries the previous anonymous handoff and reloads flags; same-ID anonymous promotion reloads only when properties were supplied; repeated identified-user property updates do not require an immediate reload. Merge flag property inputs set-once first and set last. Reset clears the eventual user-scoped flag context and reloads for fresh anonymous identity. Add these effects to the existing state transition owner rather than adding another identity store.

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

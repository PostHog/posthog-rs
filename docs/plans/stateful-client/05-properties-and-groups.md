# PR 5 — Registered properties and groups

Status: planned. Predecessor: [PR 4](04-consent.md). Follow [the shared contracts](README.md).

## Outcome and scope

Add register/unregister, remembered group membership, group identification/properties, and their event enrichment/persistence/reset behavior.

These are client context, not a new transport facility. Feature-flag targeting overrides and reload behavior arrive in PR 8. Do not prematurely mirror every analytics property into targeting state.

## Precedent and contracts

- JS: persisted register/unregister primitives in the stateless base; current groups and enriched capture in `PostHogCore`.
- Android: stateful register/group methods and shared event assembly.
- Specs: register, unregister, group, group-identify, capture, reset, set-person-properties.
- Do not assume group membership has the same person-processing implications across old implementations; check current specs and v1 processing options.

## Behavioral requirements

- Mutations affect memory before return and use the existing async state store.
- Use Rust's existing stateless implementation as the canonical property precedence and merging behavior. Integrate platform context, registered properties, groups, and later flags/session annotations without changing its existing collision, default, hook, or wire-conversion semantics.
- Validate scalar/object/array/null handling, removal semantics, and copy ownership. Caller mutation after an API returns must not change queued events or stored state.
- Group membership and group-property updates are distinct operations even if a public convenience API combines them.
- Define clearing/replacing a group and whether properties trigger an event when membership is unchanged.
- Hooks see the assembled event. Do not re-add scrubbed properties in transport.
- Already admitted events retain prior groups/properties across later updates and reset.
- Reset clears registered properties and remembered groups but preserves the queue.
- Apply each capability's consent/profile rules to group operations and event production. Purely local register/unregister changes remain allowed while opted out; they do not emit analytics.
- Bound input and persisted state; rejected oversized updates leave a coherent prior state rather than partially updating it.

## Implementation tasks

- [ ] Extend the shared assembly path for stateful context using the existing Rust stateless merge and reserved-property semantics; avoid a competing merge implementation.
- [ ] Implement register/unregister and group membership/update APIs.
- [ ] Construct group events through the common pipeline.
- [ ] Extend state serialization and reset with these domain fields.
- [ ] Update internal context revision when membership changes so PR 8 can attach flag invalidation without another source of truth.
- [ ] Document mutation versus event-admission outcomes and property ownership.
- [ ] Extend public examples and wire tests.

## Required tests

- Like-for-like base/head tests preserve Rust stateless precedence for ordinary, nested, reserved, null, group, and per-event properties through final wire conversion; add collisions involving the new stateful layers.
- Register/unregister and group state are immediately visible while persistence is delayed.
- Restart after drain restores context; reset clears it.
- Group events have correct type/key/properties and profile-processing behavior.
- Same membership/new properties, replacement, removal, invalid and oversized inputs.
- Opted-out behavior for local setters versus group event production matches the selected contract.
- Hook filtering remains effective after wire conversion/retry.
- Captures racing with group/reset observe coherent snapshots.
- Old queued events retain old context while new captures use the latest state.
- Server explicit groups and instance properties retain existing semantics.

## Done gate and handoff

- [ ] One enrichment path and one persistent state owner.
- [ ] Merge semantics have Rust-baseline regression tests; reset/consent semantics have capability-spec tests.
- [ ] PR 8 can derive current group context directly without duplicating storage.
- [ ] Record API/default decisions and exact validation evidence.

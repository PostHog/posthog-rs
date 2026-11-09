# PR 5 — Registered properties and groups

Status: implemented, routine-reviewed and release-validated locally; full-domain review and publication pending. Predecessor: [PR 4](04-consent.md). Follow [the shared contracts](README.md).

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
- `group(type, key, optional_properties)` replaces one remembered membership and attempts `$groupidentify` on every valid call, even with unchanged membership and no properties. `group_identify(type, key, properties)` updates group metadata without changing remembered membership. Both use the current client distinct ID and nest profile properties under `$group_set`.
- Explicit group operations no-op while opted out or configured with `PersonProfiles::Never`. Otherwise they enable person processing without identifying the user. Existing per-event groups do not enable processing.
- `remove_group(type)` removes one remembered membership without an event or disabling previously enabled processing. It remains available while opted out or configured with `Never`; disabled/closed clients still no-op.
- Group membership and processing changes survive hook/queue rejection without rollback or automatic event replay. Mutations return `()`; persistence and event delivery remain asynchronous.
- Hooks see the assembled event. Do not re-add scrubbed properties in transport.
- Already admitted events retain prior groups/properties across later updates and reset.
- Reset clears registered properties and remembered groups but preserves the queue.
- Apply each capability's consent/profile rules to group operations and event production. Purely local register/unregister changes remain allowed while opted out; they do not emit analytics.
- Bound input and persisted state; rejected oversized updates leave a coherent prior state rather than partially updating it.

## Implementation tasks

- [x] Extend the shared assembly path for stateful context using the existing Rust stateless merge and reserved-property semantics; avoid a competing merge implementation.
- [x] Implement synchronous void register/unregister and group/group_identify/remove_group APIs using owned property maps and string keys.
- [x] Construct group events through the common pipeline.
- [x] Extend state serialization and reset with these domain fields.
- [x] Update internal context revision when membership changes so PR 8 can attach flag invalidation without another source of truth.
- [x] Document mutation versus event-admission outcomes and property ownership.
- [x] Extend public examples and wire tests.

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

- [x] One enrichment path and one persistent state owner.
- [x] Merge semantics have Rust-baseline regression tests; reset/consent semantics have capability-spec tests.
- [x] PR 8 can derive current group context directly without duplicating storage.
- [x] Record API/default decisions and exact validation evidence.

Branch `feat/stateful-properties`, predecessor consent commit
`50c48eddc31c343e45294cdcc9ee93e06b374754` ([PR 344](https://github.com/PostHog/posthog-rs/pull/344)).
The five synchronous void context methods use the existing snapshot/writer and
capture pipeline. Schema 4 adds mandatory registered/group maps; versions 1–3
migrate with empty maps on later writes. Reset clears context atomically.
The process-local context revision changes with registry/membership/reset, while
identity generation continues to guard person-property deduplication.

Rust fill precedence is per-event properties, registered properties, remembered
group defaults, platform context and SDK defaults; explicit typed groups win.
Reserved objects retain one-level fill, ordinary objects remain whole values,
and existing post-hook custom-null normalization is unchanged. Direct group
profile fields remain nested under `$group_set`. Flag targeting/reloads remain PR 8.

Local implementation gate: `bash /Users/db/dev/worktrees/posthog-rs-stateful-evidence/verify-phase5-properties.sh`.
Logs `phase5-properties-validation/20261109T194048Z-61055` under that evidence root:
29 successful commands, libraries 459 stateful / 411 blocking-stateful / 346 server;
public context tests 19 / 18, with five repetitions per mode; existing public
capture/identity/consent/init/drain tests, gated storage, server capture, Clippy,
Rust 1.78 async/blocking and API checks passed. Canonical stateless RequestContext
controls and final wire conversion checks cover collisions; local HTTP tests cover
hook scrubbing and old queued context across restart. This is macOS execution,
not remote platform CI, live ingestion or full spec-runner conformance.

Routine review found that a small over-depth registered value could pass byte-only
memory validation but block subsequent state writes. The shared bounded encoder
and decoder now validate candidates before memory replacement, with no filesystem
access. Boundary tests cover accepted depth and whole-candidate rejection, followed
by consent/group persistence and restart. The existing decoder limit is unchanged.

The routine review correction was accepted by a fresh reviewer. Release preparation
adds minor changeset `bold-earl-goulven.md`, macOS/Windows public context-test
selection and compatibility-shim coverage of all five methods.

Full local gate: `bash /Users/db/dev/worktrees/posthog-rs-stateful-evidence/verify-phase5.sh`.
Logs `phase5-validation/20261109T194755Z-11852`: all 28 commands passed. Totals:
default 532, stateful 716, blocking 440, stateful-blocking 654, integrations 815,
workspace 724; each root run retains one existing ignored test. Shim 4/3,
provider-free TLS 3/2, API, Clippy, four Rust 1.78 modes, package, documentation
and the macOS platform command passed. Blocking documentation retains only the
established AsyncFlagPoller link warning.

Maintained loopback mock smoke passes in both modes, verifying actual gzip wire
precedence, every-call group metadata, consent guards, local removal/reset,
retained old-context retry across restart, nesting rejection and v3 migration.
Previous server/capture/identity/consent smoke coverage also passes. No live
PostHog traffic, remote platform execution or full stateful spec-runner conformance
is claimed by this local gate. Flag targeting/reload behavior remains PR 8.

Remaining: comprehensive/adversarial full-domain review, signed draft publication
and exact-head CI. No PR5 commit or publication yet.

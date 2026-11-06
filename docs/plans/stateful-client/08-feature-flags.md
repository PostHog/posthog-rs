# PR 8 — Stateful feature flags

Status: planned. Predecessor: [PR 7](07-application-lifecycle.md). Semantic dependencies: identity, consent, groups, state storage, event assembly, and sessions.

## Outcome and scope

Add current-user feature flags to the stateful client: context derivation, bootstrap, preload/reload, local cached reads, payloads, persisted cache, context invalidation, notifications, and exposure tracking.

Reuse Rust's remote protocol/decoding and result records. Do not wrap exposure-producing server getters to populate a client cache. The server's explicit-user/local-evaluation model remains unchanged.

If review size requires two PRs, split acquisition/cache from accessor/exposure semantics, keeping each usable and tested. Do not defer failure/race tests to a cleanup PR.

## Precedent and contracts

- JS `PostHogCore`: current-user context, cached getters, reload/coalescing, exposure tracking.
- Android shared client/remote-config: current-user flags and exposure behavior; server snapshots serve a different context model.
- Specs: bootstrap, identify, set-person-properties, group, reload-feature-flags, feature-flag-cache, feature-flag-called-tracker, experiment-exposure, get-feature-flag, get-feature-flag-result, get-feature-flag-payload, get-feature-flags, is-feature-enabled, on-feature-flags, person/group targeting override APIs, reset, and before-send-hook.
- Existing Rust `feature_flags.rs`, `feature_flag_evaluations.rs`, request code in both client integrations. Inspect side-effect-free export work for reuse, not as an immutable public requirement.

## Protocol scope decision before implementation

Select and record whether this milestone reads `/flags?v=4` records or defers that support. A deferral must state the supported protocol and capability limits; reusing the existing decoder alone is not evidence of current-protocol conformance.

If v4 records are supported, include config-version-2 attribution and deduplication, plus `$experiment_exposure` with its own dedup namespace. Use the evaluation details that supplied the value, require valid experiment-split metadata, and fail closed for malformed splits. Bootstrap values and client overrides do not produce direct experiment exposures. Config-version-1 reads retain their existing flag-called behavior. Include typed-accessor mismatch/default rules if those accessors are exposed.

## Behavioral contract

- Derive request context from the current distinct/anonymous identity, any supported separate device ID, group membership, and explicit targeting overrides. Specify whether/how person/group analytics updates populate those overrides.
- Getters read memory synchronously; network and cache persistence happen asynchronously.
- A conclusive disabled rich result has `enabled = false`; unavailable evaluation returns no result, not a fabricated disabled result. Preserve string variants and JSON payload types. Malformed serialized payloads use the no-payload result and diagnostics rather than returning raw malformed strings; already-decoded strings are not decoded again.
- Context changes invalidate or mark caches according to the spec; never silently serve a previous user's result as current.
- Pin evaluation context/revision at request start. A response cannot install results for an obsolete identity/group context or change consent state; opt-out alone does not make its evaluation context obsolete.
- Coalesce concurrent reloads and retain the latest requested context without unbounded queued work.
- Define same-context failure/partial/quota behavior and context-matched cache restoration.
- Flag loading, local targeting updates, and context-valid response installation remain allowed while opted out. Under PR 4's prospective consent contract, an in-flight request may complete after opt-out. Any new exposure capture checks current consent.
- Emit exposures only for eligible accesses, with the proper dedup scope and metadata. Payload-only access and side-effect-free exports must not create exposures.
- Flag-called and experiment-exposure events use the stateful capture pipeline, including hooks, consent, and session policy. Apply final minimal-event filtering only to eligible `$feature_flag_called` events; `$experiment_exposure` retains normal enrichment subject to its own prohibited-property rules.
- Notifications carry immutable result/context information and run outside state locks. Define ordering, reentrancy, and shutdown behavior.
- Cache persistence uses PR 2's store. Large optional cache data must not displace identity/consent persistence.

## Bootstrap behavior

- Serve configured bootstrap flags/payloads synchronously after initialization and before the first remote response, taking precedence over persisted flag values. Serve only boolean true or non-empty variant strings and their associated payloads.
- Notify flags-loaded listeners when bootstrap is applied and again for remote results, outside state locks.
- A complete response replaces served bootstrap values and payloads; partial or computation-error responses merge according to the spec. Do not retain an old bootstrap payload beside a newly loaded value without that payload.
- Retain bootstrap reporting context for `$feature_flag_called`. The global `$used_bootstrap_value` marker becomes false after a successful HTTP 200 response, including computation-error responses; it is not per-key provenance.
- Reset clears bootstrap values, reporting context, and the remote-response marker. Do not reapply setup bootstrap to a new user.

## Targeting updates and reloads

- Identify updates local person targeting properties even while opted out. A changed distinct ID triggers reload; same-ID transitions and property-only updates follow the identify spec's separate cases rather than reloading unconditionally.
- A changed active group triggers reload. Keep group-property overrides scoped by group type and prevent obsolete values from being attributed to a replacement group.
- Explicit person/group targeting setters merge overrides in memory before any reload, persist through the shared store, and emit no analytics. Setters default to reload where specified and support suppression; resetters clear their documented all-person or group scope.
- Before coding, record a per-operation table for identify, set-person-properties, group, and explicit targeting setters/resetters: local merge, `$set_once` handling, context revision, reload default, persistence, and reset scope. Where specs permit variation, choose and document Rust behavior. In particular, JS and Android differ on immediate reload after `set-person-properties`; do not infer one universal default.
- Where person-property mirroring is selected, apply set-once only to absent keys, then apply set values so they win. Network failure cannot roll back local overrides. Registered analytics properties are not automatically flag-targeting overrides.

## Implementation tasks

- [ ] Record the protocol scope decision and map each flag method/default to the canonical spec and peer behavior.
- [ ] Implement bootstrap serving, replacement/merge behavior, notifications, reporting, and reset.
- [ ] Complete the targeting-update/reload table with explicit choices for permitted peer variations.
- [ ] Extract the smallest shared remote result/decoder seam without duplicating request code.
- [ ] Preserve malformed-payload/quota/partial status information needed for correct cache decisions.
- [ ] Add targeting override setters/resetters, coherent context revisions, and identity/group/reset invalidation.
- [ ] Implement preload/reload and bounded coalescing with cancellation/obsolete-result handling.
- [ ] Implement local getters, rich results, cache serialization/restoration, and flags-loaded notifications.
- [ ] Implement access/exposure deduplication and privacy filtering through the common pipeline.
- [ ] Extend reset/consent/shutdown for in-flight flag work and cached state.
- [ ] Update the native example and capability acceptance adapter.
- [ ] Complete the Rust stateful milestone checks in the index; replay is separate.

## Required tests

- Public preload/reload receives values and payloads with correct distinct/anonymous/device, group, and targeting context on the wire.
- Bootstrap is immediately readable, overrides persisted flags, filters disabled/empty values and their payloads, and notifies listeners before any response.
- Complete responses replace bootstrap-only keys/payloads; partial/error responses preserve unrecomputed entries; reporting markers and reset follow the bootstrap spec.
- Opted-out identify updates identity/targeting and reloads flags without analytics; opt-in retains the new context.
- Targeting setters/resetters, set-once precedence, group changes, reload suppression/defaults, and failure-without-rollback follow the operation table.
- Concurrent reloads coalesce; identity/group changes while fetching resolve the latest context.
- Obsolete responses cannot install after identity-context changes/reset/close; opt-out alone permits context-valid completion without new exposure events.
- Failure, timeout, quota, partial results, malformed response/payload, and missing flags follow cache policy.
- Restart restores only matching-context caches; optional cache persistence failures do not corrupt mandatory state.
- Rich results distinguish known false from unavailable; test variants, valid JSON false/zero/null/empty-string payloads, malformed serialized payloads, and already-decoded strings.
- Bulk cached reads perform no network request and emit no flag-called or experiment-exposure events.
- Exposure dedup across repeated access, value/context changes, cache refresh, reset, and restart matches the approved scope.
- No exposure on payload-only reads/export; rejected admission does not permanently suppress a needed exposure.
- If v4 is supported: config-version-2 attribution, independent event deduplication, false-valued experiment assignments, malformed splits, and bootstrap/override exclusions pass wire tests; legacy records retain their behavior.
- Minimal-event filtering for eligible `$feature_flag_called` events happens after hooks without leaking disallowed properties.
- If v4 is supported: a valid experiment split with `minimalFlagCalledEvents=true` and `has_experiment=false` produces a minimized `$feature_flag_called`, while `$experiment_exposure` retains normal enrichment subject to its own prohibited-property rules.
- Callback reentrancy and shutdown do not deadlock or publish stale mutable results.
- Blocked disk backend does not delay getters or context setters; blocked network does not block capture.
- Server flag snapshots/local evaluation retain existing behavior.

## Done gate and handoff

- [ ] Stateful flag behavior passes public-entry and wire-level scenarios.
- [ ] Current-user policy has one implementation and no engine dependency.
- [ ] Full stack server regression/feature/MSRV/platform checks pass for the exact candidate.
- [ ] Native example covers identity, consent, groups, sessions, lifecycle, flags, and shutdown.
- [ ] Remaining deviations, supported platforms, storage/crash limits, and release prerequisites are explicit.

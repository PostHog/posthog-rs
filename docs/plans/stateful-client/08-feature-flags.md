# PR 8 — Stateful feature flags

Status: protocol/defaults selected; cache/schema, shared-worker transport and current-user manual reload/cache installation implemented on `feat/stateful-feature-flags`. Targeting APIs/mirroring and identity/group/reset reload triggers are implemented. Asynchronous default-true startup preload is implemented after identity reconciliation. Bootstrap cache values/payloads and transient reporting context are implemented. Event-free payload and bulk getters are implemented. Value/enablement/rich-result getters, accepted-only access reporting and final minimization are implemented. Setup/runtime notifications, explicit unsubscribe, reentrant delivery and terminal shutdown handling are implemented and reviewed. Release and whole-stack gates remain pending. Predecessor: [sessions PR 347](https://github.com/PostHog/posthog-rs/pull/347), signed head `2aac10eb141314c30f3273da47899336f8003e60`. This is the seventh implementation PR; application lifecycle analytics is deferred. Semantic dependencies: identity, consent, groups, state storage, event assembly, and sessions.

## Outcome and scope

Add current-user feature flags to the stateful client: context derivation, bootstrap, preload/reload, local cached reads, payloads, persisted cache, context invalidation, notifications, and exposure tracking.

Reuse Rust's remote protocol/decoding and result records. Do not wrap exposure-producing server getters to populate a client cache. The server's explicit-user/local-evaluation model remains unchanged.

If review size requires two PRs, split acquisition/cache from accessor/exposure semantics, keeping each usable and tested. Do not defer failure/race tests to a cleanup PR.

## Precedent and contracts

- JS `PostHogCore`: current-user context, cached getters, reload/coalescing, exposure tracking.
- Android shared client/remote-config: current-user flags and exposure behavior; server snapshots serve a different context model.
- Specs: bootstrap, identify, set-person-properties, group, reload-feature-flags, feature-flag-cache, feature-flag-called-tracker, experiment-exposure, get-feature-flag, get-feature-flag-result, get-feature-flag-payload, get-feature-flags, is-feature-enabled, on-feature-flags, person/group targeting override APIs, reset, and before-send-hook.
- Existing Rust `feature_flags.rs`, `feature_flag_evaluations.rs`, request code in both client integrations. Inspect side-effect-free export work for reuse, not as an immutable public requirement.

## Selected protocol and defaults

Use `/flags/?v=2` with the existing legacy-response compatibility. Values are booleans or string variants; payloads retain their JSON types. Access reporting uses `$feature_flag_called`, including its server-controlled minimization policy. The existing server client's protocol and getter behavior remain unchanged.

Typed v4 values, config-version-2 rule attribution and direct `$experiment_exposure` are deferred to a protocol follow-up. This milestone does not claim v4 conformance. That follow-up must implement the full record validation, attribution, independent deduplication and malformed-split rules rather than merely changing the endpoint query.

Preload runs asynchronously by default and does not delay local readiness for network success. Access-event reporting defaults to enabled, with per-read suppression. Analytics person-property updates mirror into local targeting without immediate reload, following Android/iOS behavior. Explicit targeting setters/resetters default to reload and permit suppression. Flags loading remains allowed while opted out; analytics emission checks current consent.

Pinned contract discovery: sdk-specs `96adaa788a27c765a2d54f3e4374ae5e94c204f0`, JS core `fb313fe7f5259fc104cff213aa07c24d03fbfd23`, Android `677547a0c40bfb34411132c031271bd49638f399`. These are source-inspection references, not execution or deployed-backend evidence.

## Public API and contract mapping

| Canonical capability | Rust surface | Client behavior / peer shape |
| --- | --- | --- |
| bootstrap / setup | `Config::{bootstrap_feature_flags, bootstrap_feature_flag_payloads, preload_feature_flags}` | Optional enabled-value seed; asynchronous default-true preload after final identity; matches client cache/bootstrap semantics rather than server explicit-user evaluation |
| reload-feature-flags | `Client::reload_feature_flags()` | Fire-and-forget current-user request; listeners supply completion; one active and one latest pending request |
| get-feature-flag / is-feature-enabled | `Client::{get_feature_flag, is_feature_enabled}(key, Option<bool>)` | Cached bool/variant or enablement; `None` unavailable versus known false; event override inherits `Config::send_feature_flag_event` |
| get-feature-flag-result | `Client::get_feature_flag_result(key, Option<bool>)` | Owned `FeatureFlagResult` with decoded payload and producing context/metadata; partial refresh retains each older record's provenance |
| get-feature-flag-payload | `Client::get_feature_flag_payload(key)` | Event-free decoded JSON; present null is distinct from no payload |
| get-feature-flags | `Client::{get_feature_flags, get_feature_flag_payloads}()` | Owned event-free exports; unavailable versus known-empty maps |
| on-feature-flags | `Config::on_feature_flags`, `Client::on_feature_flags` | Error-aware rich snapshot; immediate cache/status delivery, then updates; explicit idempotent `FeatureFlagsSubscription::unsubscribe` |
| set/reset-person-properties-for-flags | `Client::{set_person_properties_for_flags, reset_person_properties_for_flags}` | Merge/clear targets; optional reload defaults true; null is a value |
| set/reset-group-properties-for-flags | `Client::{set_group_properties_for_flags, reset_group_properties_for_flags}` | Overrides scoped by remembered group type; selected/all reset; optional reload defaults true |
| feature-flag-called-tracker | Value, enablement and result getters | Shared accepted-only bounded reporting; payload/bulk reads stay silent; final post-hook minimization uses the Rust allowlist |

The Rust listener belongs to one terminal Client instance, not a reusable global
close/setup facade. Subscriptions survive reset but end at shutdown. Config-time,
runtime-immediate and worker notifications can overlap; there is no UI dispatcher.
This is the documented Rust adaptation of the pinned listener specification.
The analytics-to-targeting mirroring policy below follows Android/iOS's permitted
no-immediate-reload behavior, not every JS-core call pattern. Peer references are
source comparisons; peer SDKs have not been executed as a conformance oracle.

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
- Use the per-operation table below. All effective request-context changes advance an evaluation revision independently of analytics/session state, and persist targeting changes through the existing writer. Capture rejection or network failure does not roll back targeting state.
- Where person-property mirroring is selected, apply set-once only to absent keys, then apply set values so they win. Network failure cannot roll back local overrides. Registered analytics properties are not automatically flag-targeting overrides.

### Targeting operation table

| Operation | Local merge / invalidation | Reload |
| --- | --- | --- |
| Anonymous to different identified ID | Set-once absent keys, then set; clear prior-identity results | Yes |
| Anonymous to same identified ID | Same merge; identified bit alone does not affect request context | Only when properties supplied |
| Same identified ID with properties | Same merge; preserve analytics dedup semantics | No immediate reload |
| Invalid call or different already-identified ID | Existing reset-first/validation guards; no mutation | No |
| `set_person_properties` | Set-once absent keys, then set | No immediate reload |
| Changed group membership | Clear replaced group's overrides, merge incoming properties; clear prior-group results | Yes |
| Same-group property update | Merge the selected group type | No immediate reload |
| Remove group | Remove selected membership and its overrides | When membership changed |
| Explicit targeting setters | Merge person or selected group type; empty input ignored | Default true, suppressible |
| Explicit targeting resetters | Clear all person overrides or selected/all group types | Default true, suppressible |
| Client reset | Clear targeting, cache, bootstrap reporting and access dedup | Yes, for new anonymous identity |

Property-only changes with suppressed reload retain last-known values for the same user/group until refresh. Results and notifications retain their producing evaluation context; they are not silently relabelled as evaluated with the new targeting. Older in-flight responses cannot install after a targeting revision change. Persisted cached results restore only when their full context matches restored targeting. Group replacement must not attribute the former group's overrides to the replacement.

## Implementation tasks

- [x] Select protocol scope and default preload/targeting behavior.
- [x] Complete the public flag method/API mapping to canonical specs and peers.
- [x] Implement bootstrap cache values, replacement/merge behavior, transient reporting context, bounds, and reset.
- [x] Expose bootstrap through public getters, notifications and access reporting.
- [x] Record targeting-update/reload policy, including permitted peer variations.
- [x] Extract the smallest shared remote result/decoder seam without duplicating request code.
- [x] Preserve malformed-payload/quota/partial status information needed for correct cache decisions.
- [x] Integrate current-user manual reload, stale revision/request guards, partial-record provenance and independent live-cache bounds.
- [x] Add targeting override setters/resetters, coherent context revisions, and identity/group/reset invalidation and reload triggers.
- [x] Implement preload/reload and bounded coalescing with cancellation/obsolete-result handling.
- [x] Implement local getters, rich results, cache serialization/restoration, and flags-loaded notifications.
- [x] Implement access/exposure deduplication and privacy filtering through the common pipeline.
- [x] Extend reset/consent/shutdown for in-flight flag work and cached state.
- [x] Update the native example with flags, targeting, subscriptions and explicit consent.
- [ ] Complete the Rust stateful milestone checks in the index; replay is separate.

## Follow-up: shared acceptance runner

Shared cross-SDK acceptance-runner integration is a separate follow-up, not a prerequisite for this milestone. The current compliance workflow covers the server client; stateful conformance through that runner is not established.

- [ ] Inventory the pinned harness protocol and capabilities (currently `1.26.0`); identify supported stateful scenarios and any required harness changes before implementing the adapter.
- [ ] Add an adapter mode using the public stateful client, preserving existing server-client and compression coverage.
- [ ] Map applicable bootstrap/preload, reload, cached reads/payloads, targeting, consent, reset, reporting/deduplication, subscriptions and persisted-cache scenarios to supported operations.
- [ ] Add stateful CI execution for async and blocking modes. Retain pinned inputs, rerunnable commands and a pass/fail/unsupported matrix with reasons; unsupported scenarios do not establish conformance.

Runtime API or behavior changes, application-lifecycle analytics and live-ingestion testing are outside this follow-up.

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
- [x] Native example covers identity, consent, groups, sessions, flags, and shutdown.
- [ ] Remaining deviations, supported platforms, storage/crash limits, and release prerequisites are explicit.

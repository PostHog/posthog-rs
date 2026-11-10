# PR 7 — Application lifecycle analytics

Status: deferred beyond the current stateful milestone. This document retains the proposed extension for future work; its tasks are not current stack completion gates. [Feature flags](08-feature-flags.md) follows [sessions](06-sessions.md) directly. The existing session environment signals remain session controls.

## Outcome and scope

Accept platform-neutral lifecycle inputs and produce application install/update/open/background analytics with persistent version/build bookkeeping.

Hosts detect meaningful lifecycle transitions and supply application metadata. Rust owns the reusable event/bookkeeping policy. A native Rust example can exercise the whole behavior without an engine.

This placement is a deliberate factoring decision: peers commonly implement these events in platform integrations rather than their shared stateful class. It is not a reason to move platform observers into Rust.

## Precedent and contracts

- Android: `PostHogAppInstallIntegration`, `PostHogLifecycleObserverIntegration`.
- React Native: lifecycle capture and stored version/build handling in `posthog-rn.ts`.
- Specs: application-lifecycle, capture, consent-gating, session-manager, persistent-storage, reset.
- Copy semantic outcomes, not an engine's specific callback names or Unreal's existing event/default choices.

## Decisions before coding

Define the host-facing signal vocabulary, initial foreground state, opt-in/default capture configuration, missing app version/build behavior, and update comparison semantics.

Decide when an install/update marker advances if consent blocks capture, a hook drops the event, or admission fails. Define whether enabling collection after startup produces delayed lifecycle events. These choices affect both privacy and duplicate/missing analytics and need spec/peer justification.

Memory-first persistence means exactly-once install/update reporting across hard crashes is not guaranteed. Do not introduce blocking disk writes to imply it is.

## Implementation tasks

- [ ] Define lifecycle state and distinguish initial start, repeated start, foreground resume, and background.
- [ ] Construct lifecycle events through common enrichment, hooks, consent, and admission.
- [ ] Coordinate session signals using PR 6 without a second session manager.
- [ ] Store install/version/build markers in the shared state store with installation scope.
- [ ] Extend reset tests to preserve installation markers while clearing user context.
- [ ] Make signal handling idempotent where repeated host notifications represent the same transition.
- [ ] Add an engine-free lifecycle example and document host responsibilities.

## Required tests

- Fresh installation, unchanged restart, version/build update, absent metadata.
- Duplicate start/foreground/background signals and invalid ordering.
- Foreground/background events carry correct identity/session and supplied metadata.
- Opted-out startup, later opt-in, and withdrawal during event hooks.
- Queue full or hook drop follows the approved marker-advance rule.
- Reset does not produce a new installation or erase app markers.
- Restart after drain suppresses duplicate install/update as specified.
- Hard-crash-before-write behavior is bounded/documented rather than asserted exactly-once.
- Session signals remain correct with lifecycle event capture disabled.
- Slow/failing state storage leaves caller-thread signal handling nonblocking.

## Done gate and handoff

- [ ] Policy is reusable and platform observations remain host-owned.
- [ ] Normal event controls apply; no bypass ingestion path exists.
- [ ] Installation bookkeeping and user state have explicit reset scopes.
- [ ] Documentation identifies the deliberate core/platform factoring difference.

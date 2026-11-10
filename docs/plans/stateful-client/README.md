# Stateful Rust client: implementation stack

Status: phases 1–6 are published as signed draft PRs with independent reviews and CI complete: [334](https://github.com/PostHog/posthog-rs/pull/334), [337](https://github.com/PostHog/posthog-rs/pull/337), [343](https://github.com/PostHog/posthog-rs/pull/343), [344](https://github.com/PostHog/posthog-rs/pull/344), [346](https://github.com/PostHog/posthog-rs/pull/346), and [347](https://github.com/PostHog/posthog-rs/pull/347). Application lifecycle analytics is deferred. Stateful feature flags follows sessions directly, making seven implementation PRs; original phase numbers are retained. Replay remains a separate follow-up.

## Goal and scope

Deliver an engine-neutral stateful client in the PostHog Rust SDK, alongside the existing stateless/server client. Share delivery and protocol code. The Rust client owns identity, consent, property enrichment, sessions, and current-user feature flags.

Rust correctness is the primary gate. Unreal is a later consumer, not the compatibility baseline for this API. C ABI changes, Unreal migration, engine rendering, and Unreal release infrastructure are outside this stack. Preserve existing published Rust contracts; do not inherit prerelease Unreal defaults or public shapes without an independent justification.

These plans supersede the Rust implementation sequencing and storage/consent timing in the earlier Unreal document, `posthog-unreal/docs/stateful-rust-core-plan.md`. That document remains context for eventual engine integration.

## Stack and dependencies

Each PR is based on the preceding PR for review. The semantic dependencies below explain why its domain needs earlier work; do not invent interfaces just to make every domain independently mergeable.

| PR | Domain | Main semantic dependencies | Plan |
| --- | --- | --- | --- |
| 1 | Shared delivery and event durability | Existing Rust transport | [Delivery](01-shared-delivery.md) |
| 2 | Stateful client, memory-first state store, event pipeline | 1 | [Client foundation](02-stateful-client-and-storage.md) |
| 3 | Identity and person profiles | 2 | [Identity](03-identity-and-person-profiles.md) |
| 4 | Consent | 2, 3, delivery controls in 1 | [Consent](04-consent.md) |
| 5 | Registered properties and groups | 2, 3, 4 | [Properties and groups](05-properties-and-groups.md) |
| 6 | Sessions | 2, 3, 4 | [Sessions](06-sessions.md) |
| Deferred | Application lifecycle analytics (original phase 7) | 2, 4, 6 | [Lifecycle](07-application-lifecycle.md) |
| 8 | Stateful feature flags | 2–6; directly above sessions | [Feature flags](08-feature-flags.md) |
| Follow-up | Replay policy, attribution, delivery | Stateful stack, especially 4, 6, 8 | [Replay](09-replay-follow-up.md) |

Bootstrap ownership follows the domains: PR 2 establishes anonymous bootstrap and the identity schema; PR 3 adds identified bootstrap and reconciliation; PR 8 adds flag/payload bootstrap and reporting. PR 2 also owns explicit screen-event capture; platform screen observation stays in consumers. Optional session bootstrap is a PR 6 scope decision.

The completion of PR 8 is the stateful analytics milestone. Replay is not a prerequisite. Every PR includes its own tests, documentation, API review, and diagnostics; there is no deferred correctness PR. Do not publish an intermediate stack member as the completed stateful SDK.

## Agreed storage and calling contract

1. Once initialized, client-state reads are synchronous and memory-backed.
2. State mutations update memory synchronously. Subsequent ordered calls observe the new state before the mutating call returns.
3. Disk persistence is asynchronous, ordered, and may be coalesced. Ordinary capture, identify, reset, register, and consent calls do not wait for disk or network I/O.
4. Opt-in/out changes the in-memory collection gate immediately. A successful disk write is not a prerequisite for using the new in-memory consent decision. Consent is prospective: events captured and requests started while consent was present may complete processing, persistence, retry, and delivery after opt-out, including queue recovery after restart. Opt-out blocks new collection rather than cancelling or purging previously consented work.
5. The core owns the state/schema and transition ordering. Consumers select a backend; they do not observe consent changes and implement their own persistence policy.
6. A Rust-provided file backend should serve ordinary native consumers. Platform-specific preferences backends can use the same contract. Backend calls on the synchronous path must not perform blocking file I/O.
7. Initialization loads state before the client becomes ready. Provide a nonblocking initialization route; any explicit blocking constructor must be documented as such.
8. Shutdown attempts to drain outstanding writes. Distinguish completion, timeout, and storage failure without promising that every filesystem call can be interrupted.
9. Disk failure does not roll back live state or make capture wait. Log/report failure without logging sensitive state. An abrupt crash before persistence can lose the latest state mutation; document that limit.
10. Event-queue storage is separate from client-state storage. Queue admission, state persistence, event persistence, flush attempts, and ingestion acknowledgement are different outcomes.

Concurrency uses short synchronization around state, not a requirement that every operation be lock-free. User hooks run outside state locks. Serialization costs, lock contention, and arbitrary caller hooks still count toward capture latency.

A future explicit storage drain/checkpoint may await persistence, but it is not required for ordinary operations. Prefer an internal test/shutdown barrier unless an actual public consumer requires another API.

## Cross-cutting decisions before implementation

Record these in the foundation PR design, with alternatives and the chosen contract:

- Public stateful type/configuration names and Cargo feature exposure; retain existing server entry points.
- One semantic implementation across supported blocking/async integration modes. Define initialization, completion, shutdown, and runtime ownership without duplicating state machines.
- State backend interface, namespace ownership, memory-only behavior, bounds, versioning, and initialization failures.
- Hook execution/reentrancy, recursive capture, close-from-hook behavior, and final admission validation.
- Anonymous/device identity fields and lifetimes, including whether a separate long-lived device ID is needed; do not equate an identified person ID with a device ID.
- Phase 8 protocol scope: `/flags/?v=2` with legacy-response compatibility; typed v4 values and config-version-2 direct experiment exposure are deferred.
- Supported OS/toolchain/MSRV matrix and dependency implications.
- State snapshot/transition ordering under concurrent mutation, reset, capture, and background completions.
- Public default values grounded in canonical specs and peers rather than inherited from an engine adapter.

Stop for agreement if implementing a domain requires a new public contract or architecture decision. Routine details within the agreed design do not need another approval cycle.

## Reference hierarchy and precedent

Read current repository instructions and the [SDK specification inventory](https://github.com/PostHog/sdk-specs/blob/main/README.md) before implementation. Pin the specs used by each PR. Each plan lists capability names; resolve them under `openspec/specs/<capability>/spec.md` and relevant acceptance features.

For property precedence and merging, the existing Rust stateless implementation is the canonical behavior. Reuse its semantics and wire conversion, with like-for-like regression tests. Add stateful context without changing existing merge rules; peer implementations or generic spec wording do not override this baseline.

Use three distinct evidence categories in each PR:
- **Canonical contract:** published spec and its applicable acceptance scenarios.
- **Peer precedent:** actual implementation and tests in Android or JS core.
- **Deliberate Rust design:** a new factoring, API adaptation, or stronger guarantee, with its rationale.

Node is not the stateful JS reference. Node uses `PostHogCoreStateless`; React Native uses `PostHogCore`. Android's shared `PostHog` is stateful and its server client uses `PostHogStateless`. Their queues and storage also contain mutable state.

Lifecycle analytics has peer precedent in platform integrations, not uniformly in shared stateful cores. The retained lifecycle plan is a deferred extension, not a milestone prerequisite.

### Source anchors

Resolve repositories using the canonical local SDK inventory, not copies of local paths in each phase.

- Rust: `src/client/{mod,common,transport,blocking,async_client}.rs`, `src/event.rs`, `src/capture_event.rs`, `src/feature_flags.rs`, `src/feature_flag_evaluations.rs`, `src/lib.rs`, `tests/`, `compliance/`.
- JS: `packages/core/src/posthog-core.ts`, `posthog-core-stateless.ts`; `packages/react-native/src/posthog-rn.ts`, `storage.ts`; `packages/node/src/storage-memory.ts`.
- Android: `posthog/src/main/java/com/posthog/PostHog.kt`, `PostHogStateless.kt`, `internal/PostHogQueue.kt`, `internal/PostHogSessionManager.kt`; platform storage/lifecycle integrations under `posthog-android/src/main/java/com/posthog/android/`.

## Baseline and branch preparation

The canonical local checkout was inspected at `e23676683eb60ccbfcdee31e281aaae271d2b2b2`, branch `feat/mirror-posthog-crate`. The available v1 worktree is `~/dev/worktrees/posthog-rs-posthog-v1`, at `6591a126eeb47c16785eb6a6db95e52dfd7ed339`, branch `feat/posthog-crate-v1`. Both use package name `posthog`; account for the compatibility crate when designing exports. Neither checkout location is an instruction to use that branch as the stack base.

The old `posthog-rs-replay-v1` worktree is no longer present. Its branch `feat/session-replay-v1` still points to `de45a689a5d941c70b8c4f7a503b1270dea672bd`, containing embedding work but not the previously inspected uncommitted replay implementation. Do not assume that implementation has been recovered. Existing committed embedding code is material to inspect/reuse, not a dependency to transplant wholesale.

Plan-review snapshots: JS `5663aff913ea5c6ca14dba286829cf7993bea980`, Android `bf4d8ef8c86053f34553a6b9f3c5ed4d135057ee`, specs `ffe38baa49b1616dc74d476ce884c0e24f840009`. These supersede the older comparison snapshots; refresh and pin source/spec evidence before implementation.

Before creating branches:
- Select the intended target Rust base explicitly and inventory staged/unstaged/untracked work.
- Preserve source and backup references before splitting any existing work.
- Use implementation worktrees under `~/dev/worktrees`, not disposable review directories.
- Keep one writer per branch/worktree. A domain plan is not permission to rewrite another stack member.
- Record actual base/head and predecessor PR references in each handoff. Commit/push/PR creation remain separately authorized actions.

## Common validation and review gate

Run the selected base's documented build/test/lint/API commands. Typical checks include:

```sh
cargo fmt -- --check
cargo clippy --tests -- -D warnings
cargo test --locked
cargo test --locked --no-default-features --features tls
cargo test --locked --workspace
scripts/check-public-api.sh
```

Extend these for the agreed stateful feature, supported async/blocking/TLS combinations, compatibility crate, and MSRV. They are starting points, not proof that a new feature was compiled. Keep the existing server CI matrix green. Record exact commands and results for the reviewed source.

Every domain requires:
- Deterministic unit tests for state transitions and failure paths.
- Public native Rust entry-point tests; private state assertions alone are insufficient.
- Maintained SDK mock-server wire tests where network behavior is claimed.
- Delayed/failing storage tests proving synchronous visibility and absence of caller-thread file I/O.
- Concurrent-operation tests using barriers/fake clocks, not arbitrary sleeps.
- Public API snapshot, docs/defaults, and changeset treatment per repository instructions.
- One fresh read-only review of the actual diff, callers, and tests; integrate concrete findings.

Cross-platform storage/threading tests begin with the foundation. Before declaring the Rust milestone ready, test real filesystems, clean consumer builds, resource bounds, and shutdown on each supported target. No live PostHog traffic or account mutations are required for ordinary validation.

## Progress / handoff record

PR 1 uses branch `feat/stateful-delivery`, based on `v1` at `b244fc38384e44473f4290554f347241a69fbd0f`. Its implementation and validation are recorded in the delivery plan. Add the following to each phase as work proceeds:
- Branch / base / head / predecessor PR.
- Pinned spec and peer revisions; approved intentional differences.
- Commands and result artifact locations.
- Unresolved findings, validation gaps, and next ready action.

Next action: implement stateful feature flags on `feat/stateful-feature-flags` above sessions PR 347 (`2aac10eb141314c30f3273da47899336f8003e60`). The flags plan records the selected protocol, asynchronous preload, and targeting/reload policy. Final whole-stack verification follows the flags implementation and review.

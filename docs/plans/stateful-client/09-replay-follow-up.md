# Follow-up — Replay policy, attribution, and delivery

Status: deferred until the stateful analytics milestone, except independently useful delivery fixes. Not a prerequisite for PRs 1–8.

## Outcome and scope

Provide reusable replay eligibility, attribution context, and delivery for hosts that perform their own recording. Rust owns generic policy and transport; hosts own screenshot/render acquisition, privacy masking, and encoding.

The original replay worktree is unavailable and its uncommitted implementation is not part of the retained embedding commit. Recover any source deliberately or implement from reviewed contracts; do not assume a reusable replay module exists on the stack base.

## Precedent and contracts

- Android replay queue, remote ingestion controls, session coordination, and platform capture separation.
- React Native's shared JS state plus native replay integration.
- Specs: event-batcher, session-replay-ingestion-controls, session-replay-privacy, start/stop-session-recording, is-session-replay-active, session-manager, consent-gating.
- Separate memory-only replay durability from durable analytics; choose and document replay persistence policy explicitly.

## Tasks and decisions

- [ ] Inventory the currently supported replay product controls before claiming full parity.
- [ ] Implement generic eligibility for local configuration, consent, and approved remote controls, sampling/triggers where supported.
- [ ] Provide immutable capture attribution plus validity tokens for asynchronous host work.
- [ ] Gate frame acquisition on current consent. Frames already captured under consent may finish encoding, submission, and delivery after opt-out. Define independent identity/session, recording-state, and shutdown validity checks for asynchronous host work.
- [ ] Preserve attribution on already admitted frames; never relabel frames with the current identity.
- [ ] Split every request on both session ID and distinct ID, including retries and restored data if persistence exists.
- [ ] Reuse shared networking/retry helpers where semantics match; keep replay's endpoint/wire responses distinct from analytics v1 verdicts.
- [ ] Bound queue and batch bytes, compression costs, payload sizes, retries, and shutdown.
- [ ] Define replay flush/reporting and admission guarantees rather than borrowing analytics names with different meanings.
- [ ] Apply PR 4's prospective consent policy to host capture and shared delivery; a stale capture token must not authorize new frame acquisition after opt-out.

## Required tests

- Same-session anonymous-to-identified frames create separate requests. This reproduces a defect observed through the previous native bridge.
- Session changes, interleaved identities, and retries preserve request boundaries.
- Late host frame submission after reset/session change/close follows the specified validity rules.
- Frames captured under consent can finish submission and delivery after opt-out; new acquisition while opted out is blocked.
- Admitted old-identity frames retain original attribution across consent changes.
- Batch size, queue saturation, single-frame oversize, HTTP 413 splitting, transient errors, and retry limits.
- Eligibility transitions, unavailable/malformed configuration, sampling/trigger rules, and stale flag/config responses.
- Shared shutdown budget and nonblocking reporting, with explicit loss behavior for memory-only frames.
- Public Rust synthetic-frame traffic against the maintained mock server.

## Done gate

Reusable Rust policy/delivery is proven without a renderer. Real host screenshots, privacy masking, engine lifetime, and performance require separate consumer integration tests before that consumer ships replay. They are not established by accepting synthetic frames.

# PR 6 — Sessions

Status: planned. Predecessor in stack: [PR 5](05-properties-and-groups.md). Semantic dependencies: client state, identity/reset, and consent.

## Outcome and scope

Provide reusable automatic/explicit session policy for native Rust consumers. Hosts report activity and foreground/background signals; Rust owns IDs, timestamps, expiration, and attribution.

Include session getters/read-only inspection, rotation, start/end or explicit session APIs as justified, and reset interaction. Exclude engine lifecycle observation, automatic application events, and replay capture.

## Precedent and contracts

- JS `PostHogCore`: session inactivity and maximum-duration rules, persistent session fields.
- Android shared `PostHogSessionManager`: activity, foreground/background state, session rotation and notifications.
- Specs: session-manager, get-session-id, reset, capture.
- Current peer examples use 30-minute inactivity and 24-hour maximum duration; verify current canonical defaults and exact boundary behavior before implementing.
- Instance-local Rust state is preferred to copying Android's process-global manager; explain this adaptation for multi-client correctness.

## Design decisions

Choose whether sessions survive restart, the public automatic/explicit API, valid external ID format, and whether getters count as activity. Decide whether optional session bootstrap is in scope; if supported, accept valid UUIDv7 IDs, derive session start from their timestamp, and log/fall back to a generated ID for invalid input. Specify wall-clock rollback/forward handling and the role of monotonic time for live intervals.

Define which captures count as activity. Automatic exposures, background activity, and future replay must not accidentally keep a user session alive indefinitely. A read-only context lookup must not start, rotate, or touch a session.

## Implementation tasks

- [ ] Write a transition table for sessionless/active/expired × foreground/background × automatic/explicit mode.
- [ ] Implement clock-driven policy with coherent session snapshots.
- [ ] Integrate session attachment before hooks and retain recorded IDs during delivery/retry.
- [ ] Extend reset so subsequent events use the approved fresh-session behavior atomically with new identity.
- [ ] Add host signal entry points and context revision/notification support only where needed by demonstrated consumers.
- [ ] Persist session state through the shared store if restart continuity is selected; do not add another write worker.
- [ ] Document session activity, defaults, external IDs, and per-instance ownership.

## Required tests

- First activity creates a session and later activity reuses it.
- Immediately before, at, and after inactivity/max-duration boundaries.
- Continuous activity still reaches maximum duration.
- Foreground/background transitions, extended backgrounding, and sessionless explicit mode.
- Read-only inspection neither extends nor creates/rotates a session.
- Clock rollback/forward and restored state follow the approved policy.
- If session bootstrap is supported, valid UUIDv7 continuation and invalid-ID fallback follow the bootstrap spec.
- Reset yields coherent new identity/session; old queued events keep old attribution.
- Consent changes and automatic captures do not create unintended activity.
- Concurrent reads/capture/reset observe complete sessions.
- Two clients have independent sessions; slow storage never blocks session operations.

Use injected clocks and deterministic scheduling rather than long sleeps. Verify session wire fields through the public Rust client as well as unit tests.

## Done gate and handoff

- [ ] Session behavior is independent of engine APIs and platform globals.
- [ ] Boundaries and activity sources are explicit and fully tested.
- [ ] PR 7 can report lifecycle signals without implementing timeout arithmetic.
- [ ] Future replay can obtain attribution without extending sessions.

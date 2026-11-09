# PR 6 — Sessions

Status: implemented with accepted routine/correction review, full local feature/package validation and multi-process mock smoke; full-domain review and remote CI pending on `feat/stateful-sessions`, above signed PR 5 head `2e1f55812a8583bb2ac76545157b28e8dc0dd505`. Predecessor in stack: [PR 5](05-properties-and-groups.md). Semantic dependencies: client state, identity/reset, and consent.

## Outcome and scope

Provide reusable automatic/explicit session policy for native Rust consumers. Hosts report activity and foreground/background signals; Rust owns IDs, timestamps, expiration, and attribution.

Include session getters/read-only inspection, rotation, start/end or explicit session APIs as justified, and reset interaction. Exclude engine lifecycle observation, automatic application events, and replay capture.

## Precedent and contracts

- JS `PostHogCore`: session inactivity and maximum-duration rules, persistent session fields.
- Android shared `PostHogSessionManager`: activity, foreground/background state, session rotation and notifications.
- Specs: session-manager, get-session-id, reset, capture.
- Inspected peers use 30-minute inactivity and 24-hour maximum duration. Rust uses Android's inclusive expiry boundaries and a maintaining getter that does not touch valid activity.
- Instance-local Rust state avoids coupling independent clients through Android's process-global manager.

## Selected design

Sessions are instance-local and share the existing client-state owner. `SessionPersistence::{Memory, Persistent}` defaults to Memory, independently of `SessionMode::{Automatic, Manual}` and the configured identity/event storage backend. Rust owns optional session persistence through the existing writer, including ID, start and activity timestamps, restore validation and expiry. Hosts select policy and drain before exit when durability matters; ordinary operations remain memory-first and asynchronous persistence can lose recent updates on abrupt exit. Selecting Persistent does not override an explicitly memory-only backend.

Automatic mode defaults to 30-minute inactivity and 24-hour maximum duration, expiring at or beyond each boundary. Live intervals use monotonic time; persistent restart checks use stored wall-clock timestamps. Unusable restored timing, including rollback that would extend a session, requires a fresh session. Memory mode starts sessionless after initialization and does not persist live session fields with unrelated state changes. Manual mode leaves creation and boundaries to the host and disables automatic expiration.

The synchronous surface comprises `session_id() -> Option<Uuid>`, `peek_session_id() -> Option<Uuid>`, `start_session()`, `end_session()`, `reset_session()`, `session_activity()` and `set_app_in_background(bool)`. Generated IDs are UUIDv7. The maintaining getter creates/rotates automatic foreground sessions without touching a valid session's activity; pure peek returns stored attribution without maintenance or I/O. Start is idempotent, end clears, and session reset forces a new ID. Background state starts false and repeated identical signals do not count as activity.

Valid foreground public capture/screen attempts count as activity before hooks/admission; a dropped hook or full queue does not undo it. Invalid, recursive, disabled, closed and opted-out capture calls do not update session activity. SDK-generated identity/group events and automatic exposures are passive: attach a valid existing session without creating, rotating or touching it. Background captures do not extend sessions. Per-event session properties retain canonical Rust precedence and do not replace manager state.

Opt-out prevents session creation/rotation/activity, without purging admitted event attribution or the stored session. Pure peek, explicit end and environmental foreground/background bookkeeping remain local operations. Opt-in alone is not activity. Identity reset atomically clears the session with user context while preserving foreground/background state. Automatic mode creates on the next permitted foreground operation; manual mode requires explicit start. Identify alone does not rotate.

### Transition table

Enabled, open and consented instances follow this table. Passive SDK events only inspect valid automatic sessions or stored manual sessions; pure peek never changes state.

| Mode / stored state | Foreground activity / maintaining getter | Background capture / maintaining getter | Start | End | Session reset |
| --- | --- | --- | --- | --- | --- |
| Automatic / absent | Create | Remain absent | Create | Absent | New ID |
| Automatic / valid | Reuse; only activity touches | Reuse without touch | Reuse without touch | Absent | New ID |
| Automatic / expired | Rotate | Clear without replacement | No-op; later maintenance checks expiry | Absent | New ID |
| Manual / absent | Remain absent | Remain absent | Create | Absent | New ID |
| Manual / active | Reuse without automatic aging | Reuse without automatic aging | Reuse | Absent | New ID |

Entering background touches only a still-valid existing automatic session; resuming applies foreground activity policy. Neither transition updates session clocks while opted out. PR 7 reuses these transitions even when lifecycle analytics are disabled.

## Implementation tasks

- [x] Write a transition table for sessionless/active/expired × foreground/background × automatic/explicit mode.
- [x] Implement clock-driven policy with coherent session snapshots.
- [x] Integrate session attachment before hooks and retain recorded IDs during delivery/retry.
- [x] Extend reset so subsequent events use the approved fresh-session behavior atomically with new identity.
- [x] Add host signal entry points; existing consumers do not require notifications.
- [x] Implement opt-in session persistence through the shared store, bounded schema migration and bookkeeping capacity reservation; do not add another write worker.
- [x] Document session activity, defaults, per-event IDs, and per-instance ownership.

## Required tests

- First activity creates a session and later activity reuses it.
- Immediately before, at, and after inactivity/max-duration boundaries.
- Continuous activity still reaches maximum duration.
- Foreground/background transitions, extended backgrounding, and sessionless explicit mode.
- Read-only inspection neither extends nor creates/rotates a session.
- Clock rollback/forward and restored state follow the selected policy.
- Persistent sessions continue across short process restarts and expire at the boundaries; memory sessions start fresh without changing retained queue attribution.
- Full registered state still permits session bookkeeping and consent transitions; invalid/future files preserve the existing degraded-storage behavior.
- Reset yields coherent new identity/session; old queued events keep old attribution.
- Consent changes and automatic captures do not create unintended activity.
- Concurrent reads/capture/reset observe complete sessions.
- Two clients have independent sessions; slow storage never blocks session operations.

Use injected clocks and deterministic scheduling rather than long sleeps. Verify session wire fields through the public Rust client as well as unit tests.

## Done gate and handoff

- [x] Session behavior is independent of engine APIs and platform globals.
- [x] Boundaries and activity sources have injected-clock, public-entry and storage coverage.
- [x] PR 7 can report lifecycle signals without implementing timeout arithmetic.
- [x] Pure peek provides attribution without extending sessions.

Local validation covers default, stateful, blocking, error-tracking/TLS, MCP/Tower and workspace builds, Rust 1.78, API/compatibility checks, packaging and documentation. The maintained loopback mock verifies separate-process persistent continuity, memory-mode fresh sessions, retained old-session recovery, consent, expiry/future timing, v4 migration and manual control. The CLI example continues one session across two invocations. macOS/Windows CI selects public session tests in both modes; local macOS execution does not establish remote platform success, live ingestion or complete stateful spec-runner conformance.

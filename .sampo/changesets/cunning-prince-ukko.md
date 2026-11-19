---
cargo/posthog: minor
---

- Bound stateful SDK-controlled shutdown and Drop waiting by one shared first deadline. Add `DeliveryOutcome::TimedOut` for unfinished delivery; `Stopped` means delivery workers actually finished. Already-started work retains its resources until completion, potentially indefinitely if it never returns. Destructors of application objects captured by callbacks may block outside the waiting bound.
- Keep event persistence independent of blocked uploads and configuration/flags requests. Bound resident staging, recovery and fallback plus one upload view, with exact disk accounting and at most one replacement-record overage until reclamation succeeds.
- Preserve stable device identity in host-independent storage namespaces. Replace corrupt current-format core state through normal persistence while protecting inaccessible/future data and retaining valid identity when optional caches fail.
- Commit ordinary session activity only after local admission and measure session limits with wall-clock time. Preserve pre-hook identity/session attribution.
- Replace complete flag snapshots with successful records, retain partial-response provenance, and deduplicate accepted key/typed-value reports across reloads and targeting until identity changes, reset or shutdown. Include canonical campaign/click-ID attribution in minimized flag events.
- Preserve already-admitted replay work and apply remote mobile recording quota to acquisition permission.

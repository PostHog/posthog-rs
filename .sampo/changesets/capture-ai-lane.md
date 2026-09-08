---
cargo/posthog-rs: minor
---

Add `capture_ai` for LLM analytics events, mirroring every capture entry point: `Client::capture_ai`, `capture_ai_batch`, `capture_ai_immediate`, `capture_ai_batch_immediate`, and the global `posthog_rs::capture_ai`.

AI events ride their own lane to `POST /i/v1/ai/events` (`Endpoint::CaptureAi`): a separate background worker, queue, and retry state, started on the first `capture_ai*` call, so multi-MB AI events never delay analytics events. The lane always sends zstd-compressed bodies, batches by size (about 5 MiB per request), and drops an event locally — logging only its name and byte size — when its serialized properties exceed the backend's 8 MiB ceiling. The immediate variants send oversize events and return the backend's `ai_event_too_big` verdict instead.

The SDK never inspects the event name. The backend accepts only its AI event names on the AI endpoint and reports anything else as a per-event `drop`; `capture` never reroutes `$ai_*` names either.

- `CaptureFailure::endpoint()` reports which lane a failure came from (`Endpoint::Capture` or `Endpoint::CaptureAi`).
- `Endpoint` now derives `Copy`, `PartialEq`, and `Eq`.
- A `2xx` capture response whose per-event verdicts leave events unpersisted now logs one aggregate warning per batch when no `on_error` hook is registered, on both lanes. Previously such drops were silent without a hook.

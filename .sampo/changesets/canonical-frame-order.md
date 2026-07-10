---
cargo/posthog-rs: minor
---

Error tracking stack frames now ship in canonical wire order — `$exception_list[].stacktrace.frames` is bottom-up, so `frames[0]` is the outermost frame (the entry point, e.g. `main`) and the last frame is the crash/capture site. This matches the other PostHog SDKs and the server-side native symbolication contract, which assumes bottom-up input. The whole flattened stack is globally bottom-up, including client-side inline expansion: within a single physical frame the outermost logical layer comes first and the inlined leaf comes last. Frame trimming now drops the outermost frames from the front and keeps the ones nearest the crash site. This is a breaking change to the wire order; `$exception_list` ordering itself is unchanged (`[0]` is still the outermost error with the `exception_id`/`parent_id` links).

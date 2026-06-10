---
cargo/posthog-rs: patch
---

Three fixes to the unstable `capture-v1` pipeline (off by default):

- Accept any 2xx HTTP status as success on V1 capture responses instead of exactly 200, so a future 201/202/207 from capture is not misclassified as a connection error. Malformed bodies on 2xx still surface as `Error::Serialization`.
- Send the canonical SDK identity `posthog-rs/<version>` (previously `posthog-rust/<version>`) in the `posthog-sdk-info` and `user-agent` headers. The name segment now matches the `$lib` value the v0 path sends, so capture-side `$lib`/`$lib_version` materialization attributes V1 traffic correctly in SDK Health, usage reports, and Library columns.
- Route `$feature_flag_called` events through the V1 analytics endpoint when `capture-v1` is enabled (previously they always took the legacy v0 path, splitting the pipeline). Shipping stays fire-and-forget with a single attempt and no retry loop, matching the v0 flag-event semantics on both the async and blocking clients.

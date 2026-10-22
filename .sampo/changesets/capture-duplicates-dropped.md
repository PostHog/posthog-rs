---
cargo/posthog-rs: minor
---

Report event copies that capture dropped as duplicates. When a request contains one uuid more than once, capture keeps the first event and drops the later copies. The SDK no longer resends those copies, and counts each one as lost in `on_error` (`drop/duplicate_event_uuid` in `CaptureFailure::verdict_summary`) and in `CaptureSummary::not_persisted`. The new `EventResult::duplicates_dropped` field gives the number of copies dropped for that uuid.

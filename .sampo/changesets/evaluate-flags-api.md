---
cargo/posthog-rs: minor
---

Add `evaluate_flags(distinct_id, options)` for single-call snapshot-based feature flag evaluation. Returns a `FeatureFlagEvaluations` whose `is_enabled` / `get_flag` / `get_flag_payload` methods read from the cached evaluation. `is_enabled` and `get_flag` fire deduplicated `$feature_flag_called` events with full metadata (`$feature_flag_id`, `$feature_flag_version`, `$feature_flag_reason`, `$feature_flag_request_id`). Pass the snapshot to `Event::with_flags(&snapshot)` to attach `$feature/<key>` and `$active_feature_flags` to a captured event without an extra `/flags` call.

Deprecates the legacy single-flag methods in favor of `evaluate_flags()`:

- `Client::get_feature_flag`
- `Client::is_feature_enabled`
- `Client::get_feature_flag_payload`

The methods continue to work but emit a `#[deprecated]` compile warning pointing at `evaluate_flags()`. They will be removed in a future major version.

# posthog-rs

## 0.7.0 — 2026-05-05

### Minor changes

- [db601db](https://github.com/posthog/posthog-rs/commit/db601db2a0561d55954993daf1d0cfe22853c1a5) feat(flags): support group-targeted and mixed-targeting feature flags in local evaluation
  
  Adds local evaluation support for pure group flags (where `aggregation_group_type_index` is set at the flag level) and mixed-targeting flags (where individual conditions can override the aggregation per condition). `LocalEvaluator::evaluate_flag`, `evaluate_flag_simple`, and `evaluate_all_flags` now take `groups` and `group_properties` parameters; `match_feature_flag` and `match_feature_flag_with_context` have updated signatures. Backwards-incompatible at the public-API level — see PR description for migration notes. — Thanks @patricio-posthog!

## 0.6.0 — 2026-05-01

### Minor changes

- [7950daf](https://github.com/posthog/posthog-rs/commit/7950dafc9060d7b258b3d12997edb5176134a547) Add `evaluate_flags(distinct_id, options)` for single-call snapshot-based feature flag evaluation. Returns a `FeatureFlagEvaluations` whose `is_enabled` / `get_flag` / `get_flag_payload` methods read from the cached evaluation. `is_enabled` and `get_flag` fire deduplicated `$feature_flag_called` events with full metadata (`$feature_flag_id`, `$feature_flag_version`, `$feature_flag_reason`, `$feature_flag_request_id`). Pass the snapshot to `Event::with_flags(&snapshot)` to attach `$feature/<key>` and `$active_feature_flags` to a captured event without an extra `/flags` call.
  
  When `flag_keys` is provided and local evaluation already covers every requested key, `evaluate_flags` skips the `/flags` round-trip entirely. If the remote `/flags` call fails after some flags were resolved locally, the snapshot is still returned with the local results (degraded), with `errors_while_computing_flags` propagated to subsequent `$feature_flag_called` events. String-encoded `metadata.payload` values from `/flags?v=2` are normalized to parsed JSON.
  
  Deprecates the legacy single-flag methods in favor of `evaluate_flags()`:
  
  - `Client::get_feature_flag`
  - `Client::is_feature_enabled`
  - `Client::get_feature_flag_payload`
  
  The methods continue to work but emit a `#[deprecated]` compile warning pointing at `evaluate_flags()`. They will be removed in a future major version. — Thanks @dmarticus!

## 0.5.3 — 2026-04-21

### Patch changes

- [c544e14](https://github.com/posthog/posthog-rs/commit/c544e145039ab7b6c9a4843983291d5cf44f7f70) feat(flags): switch local evaluation polling from `/api/feature_flag/local_evaluation` to `/flags/definitions` — Thanks @patricio-posthog!

## 0.5.2 — 2026-04-21

### Patch changes

- [db48ade](https://github.com/posthog/posthog-rs/commit/db48adecb5b6fdaa9544fe15bc002dd3dc06909a) Trim surrounding whitespace from API keys and host config before using them. — Thanks @marandaneto!

## 0.5.1 — 2026-04-14

### Patch changes

- [543ca47](https://github.com/posthog/posthog-rs/commit/543ca47c0d1c1365ca13a1882ea7089851bef19a) Update reqwest from 0.11.3 to 0.13.2 to replace the unmaintained feature "rustls-tls" with "rustls" (RUSTSEC-2025-0134) — Thanks @marandaneto!

## 0.5.0 — 2026-03-27

### Minor changes

- [842bb73](https://github.com/posthog/posthog-rs/commit/842bb73c17c9fce63df04d01d93a0c78b91e1a63) Add `local_evaluation_only` option to prevent remote API fallback when local evaluation is inconclusive — Thanks @dmarticus!

## 0.4.7 — 2026-03-20

Note: 0.4.4 - 0.4.6 were never released due to a bug in our release process. Commit signing is hard!

### Patch changes

- [6a04431](https://github.com/posthog/posthog-rs/commit/6a04431ecd66d85360500e15dbc28a19bb349d21) Sign commits during release process — Thanks @marandaneto!

## 0.4.3 — 2026-03-05

### Patch changes

- [b1b109d](https://github.com/posthog/posthog-rs/commit/b1b109dcdc52d9a0fd72268a44b3e367e593d8b1) Add semver comparison operators for local feature flag evaluation — Thanks @dmarticus!

## 0.4.2 — 2026-02-23

### Patch changes

- [d94ecbb](https://github.com/posthog/posthog-rs/commit/d94ecbb4e6960e775f18f7b81664c18fa35ddc12) Historical batch capture support — Thanks @z0br0wn!
- [6af1786](https://github.com/posthog/posthog-rs/commit/6af178641092740ac1cca24f08d1a1fc760f2cb1) Add Capture API response handling — Thanks @z0br0wn!
- [3fdab70](https://github.com/posthog/posthog-rs/commit/3fdab70a3a6b2310ed1f7772cf742b184104bedf) Generate (and allow overrides of) event UUID, allow for properties pass through. — Thanks @z0br0wn!

## 0.4.1 — 2026-02-18

### Patch changes

- [c15b195](https://github.com/posthog/posthog-rs/commit/c15b195728be26de67a66d64d04ada7e3b729351) Migrate release process to Sampo for automated versioning, changelogs, and publishing. — Thanks @rafaeelaudibert!

## 0.4.0 - 2026-02-04

- Add feature flags with local evaluation support (#36)
- Add group support and anonymous event support (#22)
- Add global disable function (#20)
- Add global client functions (#19)
- Add timestamp setter (#30)

## 0.2.0

- Add generic properties (#1)
- Derive Debug, PartialEq, and Eq on Event

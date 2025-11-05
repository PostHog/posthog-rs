## 0.5.0 - 2025-11-05

### Added

- Feature flags support (boolean, multivariate, payloads)
- Local evaluation for 100-1000x faster flag evaluation with background polling
- Automatic `$feature_flag_called` event tracking with deduplication
- Property-based targeting and group (B2B) support
- New methods: `is_feature_enabled()`, `get_feature_flag()`, `get_feature_flags()`, `get_feature_flag_payload()`

#### New Dependencies:

- Added `sha1` for flag matching algorithms
- Added `regex` for property matching in feature flags
- Added `tokio` (optional) for async local evaluation with background polling
- Added `json` and `gzip` features to `reqwest` for flag payloads and compression
- Dev dependencies: `httpmock` for testing, `futures` for async tests

## 0.2.6 - 2025-01-08



## 0.2.5 - 2025-01-08



##  - 2025-01-08

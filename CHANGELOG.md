## 0.6.0 - 2025-11-05

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

## 0.5.0 - 2025-11-05

### Minor Changes

Configuration system now accepts base URLs instead of full endpoint URLs
- Provide just the hostname (e.g., `https://eu.posthog.com`)
- SDK automatically appends `/i/v0/e/` for single events and `/batch/` for batch events
- Old format with full URLs still works - paths are automatically stripped and normalized
- Enables simultaneous use of both single-event and batch endpoints
## 0.4.0 - 2025-11-05

### Minor Changes

 - Refactored error handling to use organized error types (`TransportError`, `ValidationError`, `InitializationError`) with structured data (timeouts, status codes, batch sizes) that can be pattern matched
    - Existing errors will continue to work with deprecation warnings. 

 - New helper methods:
    - `is_retryable()` - identifies transient errors (timeouts, 5xx, 429)  
    - `is_client_error()` - identifies 4xx errors

#### New Dependencies:
 - Added `thiserror` to reduce writing manual error handling boilerplate

## 0.2.6 - 2025-01-08



## 0.2.5 - 2025-01-08



##  - 2025-01-08

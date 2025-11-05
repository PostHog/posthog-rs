## 0.3.0 - 2025-11-05

### Minor Changes

Refactored error handling to use structured error types (`TransportError`, `ValidationError`, `InitializationError`) instead of generic strings. Errors now include structured data (timeouts, status codes, batch sizes) that can be pattern matched.

New helper methods:
- `is_retryable()` - identifies transient errors (timeouts, 5xx, 429)  
- `is_client_error()` - identifies 4xx errors

Existing code will continue to work with deprecation warnings

## 0.2.6 - 2025-01-08



## 0.2.5 - 2025-01-08



##  - 2025-01-08

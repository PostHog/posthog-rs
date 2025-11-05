## 0.4.0 - 2025-11-05

### Minor Changes

 - Refactored error handling to use structured error types (`TransportError`, `ValidationError`, `InitializationError`) with structured data (timeouts, status codes, batch sizes) that can be pattern matched
    - Existing errors will continue to work with deprecation warnings. 

 - New helper methods:
    - `is_retryable()` - identifies transient errors (timeouts, 5xx, 429)  
    - `is_client_error()` - identifies 4xx errors

#### New Dependencies:
 - Added `thiserror` to reduce writing manual error handling boilerplate

## 0.2.6 - 2025-01-08



## 0.2.5 - 2025-01-08



##  - 2025-01-08

/// Backward compatibility tests for the PostHog Rust SDK.
///
/// This test suite verifies:
/// - Error retry logic (which errors can be retried)
/// - Error classification (client vs infrastructure errors)
/// - Backward compatibility (deprecated variants still work)
#[cfg(test)]
mod backward_compatibility_tests {
    use posthog_rs::*;
    use std::time::Duration;

    // ===== Retry Logic Tests =====

    #[test]
    fn test_http_error_retry_logic() {
        // Test the actual business logic: which HTTP status codes are retryable

        // 5xx errors should be retryable
        for status in 500..600 {
            let err = Error::Transport(TransportError::HttpError(status, "".to_string()));
            assert!(err.is_retryable(), "HTTP {} should be retryable", status);
        }

        // 4xx errors should NOT be retryable, except 429
        for status in 400..500 {
            let err = Error::Transport(TransportError::HttpError(status, "".to_string()));
            if status == 429 {
                assert!(
                    err.is_retryable(),
                    "HTTP 429 should be retryable (rate limit)"
                );
            } else {
                assert!(
                    !err.is_retryable(),
                    "HTTP {} should not be retryable",
                    status
                );
            }
        }
    }

    #[test]
    fn test_http_error_client_classification() {
        // Test the actual business logic: which HTTP errors are client errors

        // 4xx are client errors (user's fault)
        for status in 400..500 {
            let err = Error::Transport(TransportError::HttpError(status, "".to_string()));
            assert!(
                err.is_client_error(),
                "HTTP {} should be a client error",
                status
            );
        }

        // 5xx are NOT client errors (server's fault)
        for status in 500..600 {
            let err = Error::Transport(TransportError::HttpError(status, "".to_string()));
            assert!(
                !err.is_client_error(),
                "HTTP {} should not be a client error",
                status
            );
        }
    }

    // ===== Real-World Use Case Tests =====

    #[test]
    fn test_retry_strategy_with_backoff() {
        // Simulates real-world retry logic with attempt limits
        fn should_retry(err: &Error, attempt: u32, max_attempts: u32) -> bool {
            err.is_retryable() && attempt < max_attempts
        }

        // Retryable errors
        let retryable = vec![
            Error::Transport(TransportError::Timeout(Duration::from_secs(30))),
            Error::Transport(TransportError::NetworkUnreachable),
            Error::Transport(TransportError::HttpError(503, "unavailable".to_string())),
            Error::Transport(TransportError::HttpError(429, "rate limit".to_string())),
        ];

        for err in retryable {
            assert!(should_retry(&err, 1, 3), "Should retry: {:?}", err);
        }

        // Non-retryable errors
        let non_retryable = vec![
            Error::Transport(TransportError::HttpError(401, "unauthorized".to_string())),
            Error::Transport(TransportError::DnsResolution("bad.host".to_string())),
            Error::Validation(ValidationError::InvalidTimestamp("bad".to_string())),
            Error::Initialization(InitializationError::MissingApiKey),
        ];

        for err in non_retryable {
            assert!(!should_retry(&err, 1, 3), "Should not retry: {:?}", err);
        }
    }

    #[test]
    fn test_error_severity_for_logging() {
        // Simulates real-world logging strategy based on error type
        fn log_level(err: &Error) -> &str {
            match (err.is_client_error(), err.is_retryable()) {
                (true, _) => "ERROR",      // Client error - user needs to fix
                (false, true) => "WARN",   // Retryable - transient issue
                (false, false) => "ERROR", // Not retryable - permanent issue
            }
        }

        // Client errors get ERROR level
        assert_eq!(
            log_level(&Error::Validation(ValidationError::InvalidTimestamp(
                "x".into()
            ))),
            "ERROR"
        );
        assert_eq!(
            log_level(&Error::Transport(TransportError::HttpError(
                400,
                "bad".into()
            ))),
            "ERROR"
        );

        // Retryable errors get WARN level
        assert_eq!(
            log_level(&Error::Transport(TransportError::Timeout(
                Duration::from_secs(1)
            ))),
            "WARN"
        );
        assert_eq!(
            log_level(&Error::Transport(TransportError::HttpError(
                503,
                "unavailable".into()
            ))),
            "WARN"
        );

        // Permanent infrastructure errors get ERROR level
        assert_eq!(
            log_level(&Error::Transport(TransportError::DnsResolution("x".into()))),
            "ERROR"
        );
    }

    // ===== Backward Compatibility: Deprecated Variants =====

    #[allow(deprecated)]
    #[test]
    fn test_deprecated_errors_with_new_methods() {
        // CRITICAL: Ensure deprecated errors work with new is_retryable() and is_client_error()
        let deprecated_errors = vec![
            Error::Connection("timeout".to_string()),
            Error::Serialization("bad json".to_string()),
            Error::AlreadyInitialized,
            Error::NotInitialized,
            Error::InvalidTimestamp("future".to_string()),
        ];

        for err in deprecated_errors {
            // Deprecated errors conservatively return false for both methods
            assert!(
                !err.is_retryable(),
                "Deprecated error should not be retryable by default"
            );
            assert!(
                !err.is_client_error(),
                "Deprecated error should not be client error by default"
            );
        }
    }

    #[allow(deprecated)]
    #[test]
    fn test_old_and_new_errors_coexist() {
        // CRITICAL: Ensure old and new error types can be handled together
        fn categorize(err: Error) -> &'static str {
            match err {
                Error::Transport(_) => "new_transport",
                Error::Validation(_) => "new_validation",
                Error::Initialization(_) => "new_initialization",
                Error::Connection(_) => "deprecated_connection",
                Error::Serialization(_) => "deprecated_serialization",
                Error::AlreadyInitialized => "deprecated_already_init",
                Error::NotInitialized => "deprecated_not_init",
                Error::InvalidTimestamp(_) => "deprecated_timestamp",
                _ => "unknown",
            }
        }

        // New errors work
        assert_eq!(
            categorize(Error::Transport(TransportError::Timeout(
                Duration::from_secs(1)
            ))),
            "new_transport"
        );

        // Old errors still work
        assert_eq!(
            categorize(Error::Connection("err".to_string())),
            "deprecated_connection"
        );
    }

    #[allow(deprecated)]
    #[test]
    fn test_deprecated_error_construction_and_matching() {
        // Verify basic construction and pattern matching still works

        // String variants
        let conn_err = Error::Connection("network failure".to_string());
        assert!(matches!(conn_err, Error::Connection(_)));
        assert!(conn_err.to_string().contains("Connection error"));

        let serial_err = Error::Serialization("invalid json".to_string());
        assert!(matches!(serial_err, Error::Serialization(_)));

        let ts_err = Error::InvalidTimestamp("future time".to_string());
        assert!(matches!(ts_err, Error::InvalidTimestamp(_)));

        // Unit variants
        let already_init = Error::AlreadyInitialized;
        assert!(matches!(already_init, Error::AlreadyInitialized));

        let not_init = Error::NotInitialized;
        assert!(matches!(not_init, Error::NotInitialized));
    }

    #[test]
    fn test_migration_path_documented() {
        // Documents the migration path from old to new error types
        // This test doesn't assert anything - it just shows the mapping

        // Old: Error::Connection → New: Error::Transport(TransportError::*)
        let _timeout = Error::Transport(TransportError::Timeout(Duration::from_secs(30)));
        let _http = Error::Transport(TransportError::HttpError(500, "error".to_string()));

        // Old: Error::Serialization → New: Error::Validation(ValidationError::SerializationFailed)
        let _serial = Error::Validation(ValidationError::SerializationFailed("err".to_string()));

        // Old: Error::AlreadyInitialized → New: Error::Initialization(InitializationError::AlreadyInitialized)
        let _already_init = Error::Initialization(InitializationError::AlreadyInitialized);

        // Old: Error::NotInitialized → New: Error::Initialization(InitializationError::NotInitialized)
        let _not_init = Error::Initialization(InitializationError::NotInitialized);

        // Old: Error::InvalidTimestamp → New: Error::Validation(ValidationError::InvalidTimestamp)
        let _timestamp = Error::Validation(ValidationError::InvalidTimestamp("err".to_string()));
    }

    // ===== Non-Exhaustive Enum Behavior =====

    #[test]
    fn test_non_exhaustive_requires_catch_all() {
        // Verifies #[non_exhaustive] works correctly - users must include catch-all
        let err = Error::Transport(TransportError::Timeout(Duration::from_secs(1)));

        match err {
            Error::Transport(_) => {}
            Error::Validation(_) => {}
            Error::Initialization(_) => {}
            _ => {} // This is required due to #[non_exhaustive]
        }
    }
}

/// These tests will fail to compile if we make breaking changes to the public API.
/// This is intentional - if you need to change these, you're making a breaking change.
///
/// Note: These tests don't need to run successfully, they just need to compile.
/// They serve as compile-time checks for API stability.

#[cfg(test)]
mod api_compatibility_tests {
    use posthog_rs::*;

    #[test]
    fn test_event_constructors_exist() {
        let _event1 = Event::new("test", "user123");
        let _event2 = Event::new_anon("test");
    }

    #[test]
    fn test_event_methods_exist() {
        let mut event = Event::new("test", "user123");
        let _ = event.insert_prop("key", "value");
        event.add_group("group", "id");
        let _ = event.set_timestamp(chrono::Utc::now());
    }

    #[test]
    fn test_client_options_builder() {
        let _options = ClientOptionsBuilder::default()
            .api_key("test".to_string())
            .build();
    }

    #[cfg(not(feature = "async-client"))]
    #[test]
    fn test_blocking_client_methods_exist() {
        // This won't actually run, but ensures the methods exist
        fn _check_blocking_client(client: &Client) {
            let event = Event::new("test", "user");
            let _: Result<(), Error> = client.capture(event);
            let _: Result<(), Error> = client.capture_batch(vec![]);
        }
    }

    #[cfg(feature = "async-client")]
    #[test]
    fn test_async_client_methods_exist() {
        // This won't actually run, but ensures the methods exist
        async fn _check_async_client(client: &Client) {
            let event = Event::new("test", "user");
            let _: Result<(), Error> = client.capture(event).await;
            let _: Result<(), Error> = client.capture_batch(vec![]).await;
        }
    }

    #[test]
    fn test_new_error_variants_exist() {
        // Ensure new error variants can be matched
        fn _handle_error(err: Error) {
            match err {
                Error::Transport(_) => {}
                Error::Validation(_) => {}
                Error::Initialization(_) => {}
                _ => {} // non_exhaustive enum
            }
        }
    }

    #[test]
    fn test_transport_error_is_retryable_method_exists() {
        let err = TransportError::Timeout(std::time::Duration::from_secs(30));
        let _is_retryable: bool = err.is_retryable();
    }

    #[test]
    fn test_error_is_retryable_method_exists() {
        let err = Error::Transport(TransportError::Timeout(std::time::Duration::from_secs(30)));
        let _is_retryable: bool = err.is_retryable();
    }

    #[test]
    fn test_error_is_client_error_method_exists() {
        let err = Error::Validation(ValidationError::InvalidTimestamp("test".to_string()));
        let _is_client_error: bool = err.is_client_error();
    }

    #[test]
    fn test_global_functions_exist() {
        // Just check they compile
        #[cfg(feature = "async-client")]
        async fn _check_async_global() {
            let _init: fn(ClientOptions) -> _ = init_global;
            let event = Event::new("test", "user");
            let _: Result<(), Error> = capture(event).await;
            let _disable: fn() = disable_global;
            let _is_disabled: fn() -> bool = global_is_disabled;
        }

        #[cfg(not(feature = "async-client"))]
        fn _check_blocking_global() {
            let _init: fn(ClientOptions) -> Result<(), Error> = init_global;
            let event = Event::new("test", "user");
            let _: Result<(), Error> = capture(event);
            let _disable: fn() = disable_global;
            let _is_disabled: fn() -> bool = global_is_disabled;
        }
    }

    #[test]
    fn test_error_display_trait() {
        // Ensure Error implements Display (for error messages)
        fn _requires_display<T: std::fmt::Display>(_: T) {}

        let err = Error::Transport(TransportError::Timeout(std::time::Duration::from_secs(30)));
        _requires_display(err);
    }

    #[test]
    fn test_error_debug_trait() {
        // Ensure Error implements Debug
        fn _requires_debug<T: std::fmt::Debug>(_: T) {}

        let err = Error::Transport(TransportError::Timeout(std::time::Duration::from_secs(30)));
        _requires_debug(err);
    }

    #[test]
    fn test_error_conversions() {
        // Test that new error types can be converted to Error
        let transport_err = TransportError::Timeout(std::time::Duration::from_secs(30));
        let _: Error = transport_err.into();

        let validation_err = ValidationError::InvalidTimestamp("test".to_string());
        let _: Error = validation_err.into();

        let init_err = InitializationError::MissingApiKey;
        let _: Error = init_err.into();
    }

    #[test]
    fn test_client_options_from_str() {
        // Test that ClientOptions can be constructed from &str
        let _options: ClientOptions = "test_api_key".into();
    }
}

/// Runtime error API stability tests.
/// These tests verify that error handling behavior remains stable across versions.
#[cfg(test)]
mod runtime_error_stability_tests {
    use posthog_rs::*;
    use std::time::Duration;

    // ===== TransportError Runtime Tests =====

    #[test]
    fn test_transport_error_timeout_construction() {
        let err = TransportError::Timeout(Duration::from_secs(30));
        assert!(err.is_retryable());
    }

    #[test]
    fn test_transport_error_network_unreachable_construction() {
        let err = TransportError::NetworkUnreachable;
        assert!(err.is_retryable());
    }

    #[test]
    fn test_transport_error_dns_construction() {
        let err = TransportError::DnsResolution("example.com".to_string());
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_transport_error_http_error_construction() {
        let err = TransportError::HttpError(404, "Not Found".to_string());
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_transport_error_tls_construction() {
        let err = TransportError::TlsError("TLS handshake failed".to_string());
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_transport_error_5xx_is_retryable() {
        let err = TransportError::HttpError(500, "Internal Server Error".to_string());
        assert!(err.is_retryable());

        let err = TransportError::HttpError(503, "Service Unavailable".to_string());
        assert!(err.is_retryable());
    }

    #[test]
    fn test_transport_error_429_is_retryable() {
        let err = TransportError::HttpError(429, "Too Many Requests".to_string());
        assert!(err.is_retryable());
    }

    #[test]
    fn test_transport_error_4xx_not_retryable() {
        let err = TransportError::HttpError(400, "Bad Request".to_string());
        assert!(!err.is_retryable());

        let err = TransportError::HttpError(401, "Unauthorized".to_string());
        assert!(!err.is_retryable());
    }

    // ===== ValidationError Runtime Tests =====

    #[test]
    fn test_validation_error_invalid_timestamp_construction() {
        let err = ValidationError::InvalidTimestamp("future timestamp".to_string());
        let error: Error = err.into();
        assert!(error.is_client_error());
        assert!(!error.is_retryable());
    }

    #[test]
    fn test_validation_error_invalid_distinct_id_construction() {
        let err = ValidationError::InvalidDistinctId("empty".to_string());
        let error: Error = err.into();
        assert!(error.is_client_error());
    }

    #[test]
    fn test_validation_error_property_too_large_construction() {
        let err = ValidationError::PropertyTooLarge {
            key: "large_prop".to_string(),
            size: 1024,
        };
        let error: Error = err.into();
        assert!(error.is_client_error());
    }

    #[test]
    fn test_validation_error_batch_size_exceeded_construction() {
        let err = ValidationError::BatchSizeExceeded {
            size: 1000,
            max: 100,
        };
        let error: Error = err.into();
        assert!(error.is_client_error());
    }

    #[test]
    fn test_validation_error_serialization_failed_construction() {
        let err = ValidationError::SerializationFailed("invalid json".to_string());
        let error: Error = err.into();
        assert!(error.is_client_error());
    }

    // ===== InitializationError Runtime Tests =====

    #[test]
    fn test_initialization_error_missing_api_key_construction() {
        let err = InitializationError::MissingApiKey;
        let error: Error = err.into();
        assert!(error.is_client_error());
        assert!(!error.is_retryable());
    }

    #[test]
    fn test_initialization_error_already_initialized_construction() {
        let err = InitializationError::AlreadyInitialized;
        let error: Error = err.into();
        assert!(error.is_client_error());
    }

    #[test]
    fn test_initialization_error_not_initialized_construction() {
        let err = InitializationError::NotInitialized;
        let error: Error = err.into();
        assert!(error.is_client_error());
    }

    #[test]
    fn test_initialization_error_invalid_endpoint_construction() {
        let err = InitializationError::InvalidEndpoint("not a url".to_string());
        let error: Error = err.into();
        assert!(error.is_client_error());
    }

    #[test]
    fn test_initialization_error_invalid_timeout_construction() {
        let err = InitializationError::InvalidTimeout(Duration::from_secs(0));
        let error: Error = err.into();
        assert!(error.is_client_error());
    }

    // ===== Error Pattern Matching Tests =====

    #[test]
    fn test_error_pattern_matching_transport() {
        let err = Error::Transport(TransportError::Timeout(Duration::from_secs(30)));

        match err {
            Error::Transport(t) => {
                assert!(t.is_retryable());
            }
            _ => panic!("Expected Transport variant"),
        }
    }

    #[test]
    fn test_error_pattern_matching_validation() {
        let err = Error::Validation(ValidationError::InvalidTimestamp("test".to_string()));

        match err {
            Error::Validation(_) => {
                // Success - can match on Validation variant
            }
            _ => panic!("Expected Validation variant"),
        }
    }

    #[test]
    fn test_error_pattern_matching_initialization() {
        let err = Error::Initialization(InitializationError::MissingApiKey);

        match err {
            Error::Initialization(_) => {
                // Success - can match on Initialization variant
            }
            _ => panic!("Expected Initialization variant"),
        }
    }

    // ===== Error Display Message Stability Tests =====

    #[test]
    fn test_transport_error_timeout_display_message() {
        let err = TransportError::Timeout(Duration::from_secs(30));
        let msg = err.to_string();
        assert!(msg.contains("timed out") || msg.contains("timeout"));
        assert!(msg.contains("30"));
    }

    #[test]
    fn test_transport_error_http_display_message() {
        let err = TransportError::HttpError(404, "Not Found".to_string());
        let msg = err.to_string();
        assert!(msg.contains("404"));
        assert!(msg.contains("Not Found"));
    }

    #[test]
    fn test_validation_error_timestamp_display_message() {
        let err = ValidationError::InvalidTimestamp("future".to_string());
        let msg = err.to_string();
        assert!(msg.contains("timestamp") || msg.contains("Invalid"));
        assert!(msg.contains("future"));
    }

    #[test]
    fn test_initialization_error_missing_key_display_message() {
        let err = InitializationError::MissingApiKey;
        let msg = err.to_string();
        assert!(msg.contains("API key") || msg.contains("missing"));
    }

    #[test]
    fn test_error_display_message_with_transport() {
        let err = Error::Transport(TransportError::Timeout(Duration::from_secs(30)));
        let msg = err.to_string();
        // With transparent error, should show inner error message
        assert!(msg.contains("timed out") || msg.contains("timeout"));
    }

    // ===== Error Method Behavior Tests =====

    #[test]
    fn test_error_is_retryable_with_transport_timeout() {
        let err = Error::Transport(TransportError::Timeout(Duration::from_secs(30)));
        assert!(err.is_retryable());
    }

    #[test]
    fn test_error_is_retryable_with_validation() {
        let err = Error::Validation(ValidationError::InvalidTimestamp("test".to_string()));
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_error_is_retryable_with_initialization() {
        let err = Error::Initialization(InitializationError::MissingApiKey);
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_error_is_client_error_with_validation() {
        let err = Error::Validation(ValidationError::InvalidTimestamp("test".to_string()));
        assert!(err.is_client_error());
    }

    #[test]
    fn test_error_is_client_error_with_initialization() {
        let err = Error::Initialization(InitializationError::MissingApiKey);
        assert!(err.is_client_error());
    }

    #[test]
    fn test_error_is_client_error_with_4xx_http() {
        let err = Error::Transport(TransportError::HttpError(400, "Bad Request".to_string()));
        assert!(err.is_client_error());
    }

    #[test]
    fn test_error_is_not_client_error_with_5xx_http() {
        let err = Error::Transport(TransportError::HttpError(500, "Server Error".to_string()));
        assert!(!err.is_client_error());
    }

    #[test]
    fn test_error_is_not_client_error_with_timeout() {
        let err = Error::Transport(TransportError::Timeout(Duration::from_secs(30)));
        assert!(!err.is_client_error());
    }

    // ===== Error Trait Implementation Tests =====

    #[test]
    fn test_error_implements_std_error_trait() {
        use std::error::Error as StdError;

        let err = Error::Transport(TransportError::Timeout(Duration::from_secs(30)));
        let _: &dyn StdError = &err;
    }

    #[test]
    fn test_error_implements_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Error>();
    }

    #[test]
    fn test_error_implements_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Error>();
    }

    // ===== Error Conversion Tests =====

    #[test]
    fn test_transport_error_converts_to_error() {
        let transport_err = TransportError::Timeout(Duration::from_secs(30));
        let error: Error = transport_err.into();
        assert!(matches!(error, Error::Transport(_)));
    }

    #[test]
    fn test_validation_error_converts_to_error() {
        let validation_err = ValidationError::InvalidTimestamp("test".to_string());
        let error: Error = validation_err.into();
        assert!(matches!(error, Error::Validation(_)));
    }

    #[test]
    fn test_initialization_error_converts_to_error() {
        let init_err = InitializationError::MissingApiKey;
        let error: Error = init_err.into();
        assert!(matches!(error, Error::Initialization(_)));
    }

    #[test]
    fn test_error_from_transport_error() {
        let error = Error::from(TransportError::Timeout(Duration::from_secs(30)));
        assert!(matches!(error, Error::Transport(_)));
    }

    #[test]
    fn test_error_from_validation_error() {
        let error = Error::from(ValidationError::InvalidTimestamp("test".to_string()));
        assert!(matches!(error, Error::Validation(_)));
    }

    #[test]
    fn test_error_from_initialization_error() {
        let error = Error::from(InitializationError::MissingApiKey);
        assert!(matches!(error, Error::Initialization(_)));
    }
}

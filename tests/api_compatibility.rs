/// API stability tests
/// These tests will fail to compile if we make breaking changes to the public API.
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

    #[test]
    fn test_client_options_from_str() {
        let _options: ClientOptions = "test_api_key".into();
    }

    #[cfg(not(feature = "async-client"))]
    #[test]
    fn test_blocking_client_methods_exist() {
        fn _check_blocking_client(client: &Client) {
            let event = Event::new("test", "user");
            let _: Result<(), Error> = client.capture(event);
            let _: Result<(), Error> = client.capture_batch(vec![]);
        }
    }

    #[cfg(feature = "async-client")]
    #[test]
    fn test_async_client_methods_exist() {
        async fn _check_async_client(client: &Client) {
            let event = Event::new("test", "user");
            let _: Result<(), Error> = client.capture(event).await;
            let _: Result<(), Error> = client.capture_batch(vec![]).await;
        }
    }

    #[test]
    fn test_global_functions_exist() {
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
    fn test_error_types_exist() {
        // Ensure error types can be constructed and matched
        fn _handle_errors() {
            let _transport = TransportError::Timeout(std::time::Duration::from_secs(30));
            let _validation = ValidationError::InvalidTimestamp("test".to_string());
            let _init = InitializationError::MissingApiKey;

            let _err1: Error = Error::Transport(TransportError::NetworkUnreachable);
            let _err2: Error = Error::Validation(ValidationError::InvalidDistinctId("".into()));
            let _err3: Error = Error::Initialization(InitializationError::NotInitialized);
        }
    }

    #[test]
    fn test_error_methods_exist() {
        let err = Error::Transport(TransportError::Timeout(std::time::Duration::from_secs(30)));
        let _: bool = err.is_retryable();
        let _: bool = err.is_client_error();
    }

    #[test]
    fn test_non_exhaustive_pattern_matching() {
        let err = Error::Transport(TransportError::Timeout(std::time::Duration::from_secs(1)));

        // Must include catch-all due to #[non_exhaustive]
        match err {
            Error::Transport(_) => {}
            Error::Validation(_) => {}
            Error::Initialization(_) => {}
            _ => {}
        }
    }
}

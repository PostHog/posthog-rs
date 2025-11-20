use posthog_rs::{ClientOptions, ClientOptionsBuilder, Error};

#[test]
fn test_client_options_builder_default_endpoint() {
    let options = ClientOptionsBuilder::new()
        .api_key("test_key")
        .build()
        .unwrap();

    assert_eq!(
        options.single_event_endpoint(),
        "https://us.i.posthog.com/i/v0/e/"
    );
    assert_eq!(
        options.batch_event_endpoint(),
        "https://us.i.posthog.com/batch/"
    );
}

#[test]
fn test_client_options_builder_with_hostname() {
    let options = ClientOptionsBuilder::new()
        .api_key("test_key")
        .api_endpoint("https://eu.posthog.com")
        .build()
        .unwrap();

    // EU PostHog Cloud redirects to EU ingestion endpoint
    assert_eq!(
        options.single_event_endpoint(),
        "https://eu.i.posthog.com/i/v0/e/"
    );
    assert_eq!(
        options.batch_event_endpoint(),
        "https://eu.i.posthog.com/batch/"
    );
}

#[test]
fn test_client_options_builder_with_full_endpoint_single() {
    // Backward compatibility: accept full endpoint and strip path
    let options = ClientOptionsBuilder::new()
        .api_key("test_key")
        .api_endpoint("https://us.i.posthog.com/i/v0/e/")
        .build()
        .unwrap();

    assert_eq!(
        options.single_event_endpoint(),
        "https://us.i.posthog.com/i/v0/e/"
    );
    assert_eq!(
        options.batch_event_endpoint(),
        "https://us.i.posthog.com/batch/"
    );
}

#[test]
fn test_client_options_builder_with_full_endpoint_batch() {
    // Backward compatibility: accept batch endpoint and strip path
    let options = ClientOptionsBuilder::new()
        .api_key("test_key")
        .api_endpoint("https://us.i.posthog.com/batch/")
        .build()
        .unwrap();

    assert_eq!(
        options.single_event_endpoint(),
        "https://us.i.posthog.com/i/v0/e/"
    );
    assert_eq!(
        options.batch_event_endpoint(),
        "https://us.i.posthog.com/batch/"
    );
}

#[test]
fn test_client_options_builder_with_port() {
    let options = ClientOptionsBuilder::new()
        .api_key("test_key")
        .api_endpoint("http://localhost:8000")
        .build()
        .unwrap();

    assert_eq!(
        options.single_event_endpoint(),
        "http://localhost:8000/i/v0/e/"
    );
    assert_eq!(
        options.batch_event_endpoint(),
        "http://localhost:8000/batch/"
    );
}

#[test]
fn test_client_options_builder_with_trailing_slash() {
    let options = ClientOptionsBuilder::new()
        .api_key("test_key")
        .api_endpoint("https://eu.posthog.com/")
        .build()
        .unwrap();

    assert_eq!(
        options.single_event_endpoint(),
        "https://eu.i.posthog.com/i/v0/e/"
    );
    assert_eq!(
        options.batch_event_endpoint(),
        "https://eu.i.posthog.com/batch/"
    );
}

#[test]
fn test_client_options_builder_invalid_endpoint_no_scheme() {
    let result = ClientOptionsBuilder::new()
        .api_key("test_key")
        .api_endpoint("posthog.com")
        .build();

    assert!(result.is_err());
    match result.unwrap_err() {
        #[allow(deprecated)]
        Error::Serialization(msg) => {
            assert!(msg.contains("Endpoint must start with http://"));
        }
        _ => panic!("Expected Serialization error"),
    }
}

#[test]
fn test_client_options_builder_invalid_endpoint_malformed() {
    let result = ClientOptionsBuilder::new()
        .api_key("test_key")
        .api_endpoint("not a url")
        .build();

    assert!(result.is_err());
    match result.unwrap_err() {
        #[allow(deprecated)]
        Error::Serialization(msg) => {
            // Should contain error about scheme or being invalid
            assert!(msg.contains("http://") || msg.contains("https://"));
        }
        _ => panic!("Expected Serialization error"),
    }
}

#[test]
fn test_client_options_builder_missing_api_key() {
    let result = ClientOptionsBuilder::new().build();

    assert!(result.is_err());
    match result.unwrap_err() {
        #[allow(deprecated)]
        Error::UninitializedField(field) => {
            assert_eq!(field, "api_key");
        }
        _ => panic!("Expected UninitializedField error"),
    }
}

#[test]
fn test_client_options_from_str() {
    let options: ClientOptions = "test_key".into();
    assert_eq!(options.api_key, "test_key");
    assert_eq!(
        options.single_event_endpoint(),
        "https://us.i.posthog.com/i/v0/e/"
    );
}

#[test]
fn test_client_options_custom_timeout() {
    let options = ClientOptionsBuilder::new()
        .api_key("test_key")
        .request_timeout_seconds(60)
        .build()
        .unwrap();

    assert_eq!(options.request_timeout_seconds, 60);
}

use posthog_rs::{ClientOptions, ClientOptionsBuilder, Error};

#[test]
fn test_client_options_builder_default_endpoint() {
    let options = ClientOptionsBuilder::new()
        .api_key("test_key".to_string())
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
        .api_key("test_key".to_string())
        .api_endpoint("https://eu.posthog.com".to_string())
        .build()
        .unwrap();

    assert_eq!(
        options.single_event_endpoint(),
        "https://eu.posthog.com/i/v0/e/"
    );
    assert_eq!(
        options.batch_event_endpoint(),
        "https://eu.posthog.com/batch/"
    );
}

#[test]
fn test_client_options_builder_with_full_endpoint_single() {
    // Backward compatibility: accept full endpoint and strip path
    let options = ClientOptionsBuilder::new()
        .api_key("test_key".to_string())
        .api_endpoint("https://us.i.posthog.com/i/v0/e/".to_string())
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
        .api_key("test_key".to_string())
        .api_endpoint("https://us.i.posthog.com/batch/".to_string())
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
        .api_key("test_key".to_string())
        .api_endpoint("http://localhost:8000".to_string())
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
        .api_key("test_key".to_string())
        .api_endpoint("https://eu.posthog.com/".to_string())
        .build()
        .unwrap();

    assert_eq!(
        options.single_event_endpoint(),
        "https://eu.posthog.com/i/v0/e/"
    );
    assert_eq!(
        options.batch_event_endpoint(),
        "https://eu.posthog.com/batch/"
    );
}

#[test]
fn test_client_options_builder_invalid_endpoint_no_scheme() {
    let result = ClientOptionsBuilder::new()
        .api_key("test_key".to_string())
        .api_endpoint("posthog.com".to_string())
        .build();

    assert!(result.is_err());
    match result.unwrap_err() {
        Error::Serialization(msg) => {
            assert!(msg.contains("Endpoint must start with http://"));
        }
        _ => panic!("Expected Serialization error"),
    }
}

#[test]
fn test_client_options_builder_invalid_endpoint_malformed() {
    let result = ClientOptionsBuilder::new()
        .api_key("test_key".to_string())
        .api_endpoint("not a url".to_string())
        .build();

    assert!(result.is_err());
    match result.unwrap_err() {
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
        Error::Serialization(msg) => {
            assert!(msg.contains("API key"));
        }
        _ => panic!("Expected Serialization error"),
    }
}

#[test]
fn test_client_options_from_str() {
    let options: ClientOptions = "test_key".into();
    assert_eq!(options.api_key(), "test_key");
    assert_eq!(
        options.single_event_endpoint(),
        "https://us.i.posthog.com/i/v0/e/"
    );
}

#[test]
fn test_client_options_custom_timeout() {
    let options = ClientOptionsBuilder::new()
        .api_key("test_key".to_string())
        .request_timeout_seconds(60)
        .build()
        .unwrap();

    assert_eq!(options.request_timeout_seconds(), 60);
}

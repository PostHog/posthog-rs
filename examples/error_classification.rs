///  Error Classification Example
///
///  Run this example (blocking/sync client only):
///  cargo run --example error_classification --no-default-features
#[cfg(feature = "async-client")]
fn main() {
    eprintln!("ERROR: This example only works with the blocking/sync client.");
    eprintln!("Run with: cargo run --example error_classification --no-default-features");
}

#[cfg(not(feature = "async-client"))]
fn main() {
    use posthog_rs::{ClientOptionsBuilder, Event};

    println!("Error Classification Examples");
    println!("───────────────────────────────────────");
    println!("Demonstrates pattern matching on specific error types with client calls");
    println!();

    use posthog_rs::{Error, TransportError, ValidationError};

    // Test different configurations to trigger various errors
    let test_cases = vec![
        (
            "DNS Resolution Error",
            "https://invalid-domain-xyz123.posthog.com",
            "phc_test",
            30,
        ),
        (
            "HTTP 401 Error (Invalid API Key)",
            "https://eu.posthog.com",
            "phc_invalid_key_demo",
            30,
        ),
        (
            "Timeout Error (Very short timeout)",
            "https://eu.posthog.com",
            "phc_test",
            0,
        ), // 0 second timeout will trigger timeout
        (
            "Network Error",
            "https://fake-endpoint-test.io",
            "phc_test",
            5,
        ),
    ];

    for (test_name, endpoint, api_key, timeout_secs) in test_cases {
        println!("Test name: {}", test_name);
        println!("───────────────────────────────────────");

        let test_options = ClientOptionsBuilder::default()
            .api_key(api_key.to_string())
            .api_endpoint(endpoint.to_string())
            .request_timeout_seconds(timeout_secs)
            .build()
            .unwrap();

        let test_client = posthog_rs::client(test_options);
        let event = Event::new("test_event", "user_123");

        match test_client.capture(event) {
            Ok(_) => println!("✓ Event sent successfully"),
            Err(e) => {
                // println!();

                // Old way (still works for backward compatibility)
                println!("OLD WAY (deprecated but backward compatible):");
                #[allow(deprecated)]
                match &e {
                    Error::Connection(msg) => {
                        println!("  Connection error string: '{}'", msg);
                        println!(
                            "  Would need to parse: if msg.contains(\"timeout\") {{ retry(); }}"
                        );
                        println!("  Fragile - breaks if error format changes!");
                    }
                    Error::Serialization(msg) => {
                        println!("  Serialization error string: '{}'", msg);
                        println!("  Would need regex to extract details from string");
                    }
                    Error::InvalidTimestamp(msg) => {
                        println!("  Invalid timestamp string: '{}'", msg);
                        println!("  Must parse timestamp format from string");
                    }
                    Error::AlreadyInitialized => {
                        println!("  Global client already initialized");
                        println!("  No additional info available");
                    }
                    Error::NotInitialized => {
                        println!("  Global client not initialized");
                        println!("  No additional info available");
                    }
                    _ => {
                        println!("  Unknown error type - can't extract details");
                        println!("  Error: {}", e);
                    }
                }
                println!();

                // New way (structured data)
                println!("NEW WAY (type-safe pattern matching):");
                match &e {
                    Error::Transport(TransportError::Timeout(duration)) => {
                        println!("  ✓ Pattern matched: TransportError::Timeout");
                        println!("  ✓ Extracted duration: {:?}", duration);
                        println!("  ✓ is_retryable(): {}", e.is_retryable());
                        println!("  ✓ is_client_error(): {}", e.is_client_error());
                        println!("  → Action: Retry with exponential backoff");
                    }
                    Error::Transport(TransportError::HttpError(401, msg)) => {
                        println!("  ✓ Pattern matched: TransportError::HttpError(401)");
                        println!("  ✓ Extracted message: {}", msg);
                        println!("  ✓ is_retryable(): {}", e.is_retryable());
                        println!("  ✓ is_client_error(): {}", e.is_client_error());
                        println!("  → Action: Check POSTHOG_API_KEY environment variable");
                    }
                    Error::Transport(TransportError::HttpError(status, msg))
                        if *status >= 400 && *status < 500 =>
                    {
                        println!("  ✓ Pattern matched: TransportError::HttpError({})", status);
                        println!("  ✓ Extracted status: {} (Client Error)", status);
                        println!("  ✓ Extracted message: {}", msg);
                        println!("  ✓ is_retryable(): {}", e.is_retryable());
                        println!("  ✓ is_client_error(): {}", e.is_client_error());
                        println!("  → Action: Fix request (client error - won't retry)");
                    }
                    Error::Transport(TransportError::HttpError(status, msg)) if *status >= 500 => {
                        println!("  ✓ Pattern matched: TransportError::HttpError({})", status);
                        println!("  ✓ Extracted status: {}", status);
                        println!("  ✓ Extracted message: {}", msg);
                        println!("  ✓ is_retryable(): {}", e.is_retryable());
                        println!("  ✓ is_client_error(): {}", e.is_client_error());
                        println!("  → Action: Retry (server error)");
                    }
                    Error::Transport(TransportError::DnsResolution(host)) => {
                        println!("  ✓ Pattern matched: TransportError::DnsResolution");
                        println!("  ✓ Extracted hostname: {}", host);
                        println!("  ✓ is_retryable(): {}", e.is_retryable());
                        println!("  ✓ is_client_error(): {}", e.is_client_error());
                        println!("  → Action: Check network/DNS configuration");
                    }
                    Error::Transport(TransportError::NetworkUnreachable) => {
                        println!("  ✓ Pattern matched: TransportError::NetworkUnreachable");
                        println!("  ✓ No parsing needed - clear error type");
                        println!("  ✓ is_retryable(): {}", e.is_retryable());
                        println!("  ✓ is_client_error(): {}", e.is_client_error());
                        println!("  → Action: Check internet connection");
                    }
                    Error::Validation(ValidationError::BatchSizeExceeded { size, max }) => {
                        println!("  ✓ Pattern matched: ValidationError::BatchSizeExceeded");
                        println!("  ✓ Extracted size: {}, max: {}", size, max);
                        println!("  ✓ Calculated chunks: {}", (size + max - 1) / max);
                        println!("  ✓ is_retryable(): {}", e.is_retryable());
                        println!("  ✓ is_client_error(): {}", e.is_client_error());
                        println!("  → Action: Split batch into chunks");
                        println!();
                        println!("  Auto-split code:");
                        println!("    for chunk in events.chunks({}) {{", max);
                        println!("        client.capture_batch(chunk)?;");
                        println!("    }}");
                    }
                    Error::Validation(ValidationError::PropertyTooLarge { key, size }) => {
                        println!("  ✓ Pattern matched: ValidationError::PropertyTooLarge");
                        println!("  ✓ Extracted property: '{}', size: {} bytes", key, size);
                        println!("  ✓ is_retryable(): {}", e.is_retryable());
                        println!("  ✓ is_client_error(): {}", e.is_client_error());
                        println!("  → Action: Truncate or remove property");
                    }
                    _ => {
                        println!("  ✓ Other error handled");
                        println!("  ✓ is_retryable(): {}", e.is_retryable());
                        println!("  ✓ is_client_error(): {}", e.is_client_error());
                    }
                }
            }
        }
        println!();
    }
}

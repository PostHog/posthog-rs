# PostHog Rust

Please see the main [PostHog docs](https://posthog.com/docs)

**This crate is under development**

# Quickstart

Add `posthog-rs` to your `Cargo.toml`.

```toml
[dependencies]
posthog-rs = "0.3.7"
```

## Basic Usage (US Region)

```rust
use posthog_rs::{Event, ClientOptionsBuilder};

// Simple initialization with API key (defaults to US region)
let client = posthog_rs::client(env!("POSTHOG_API_KEY"));

// Create and send an event
let mut event = Event::new("user_signed_up", "user_distinct_id");
event.insert_prop("plan", "premium").unwrap();
event.insert_prop("source", "web").unwrap();

client.capture(event).unwrap();
```

## EU Region Configuration

```rust
use posthog_rs::{Event, ClientOptionsBuilder};

// Configure for EU region - just provide the base URL
let options = ClientOptionsBuilder::new()
    .api_key("phc_your_api_key")
    .api_endpoint("https://eu.posthog.com")  // SDK handles /i/v0/e/ and /batch/ automatically
    .build()
    .unwrap();

let client = posthog_rs::client(options);

// Single event capture
let event = Event::new("user_signed_up", "user_distinct_id");
client.capture(event).unwrap();

// Batch event capture (uses same base URL, different endpoint path)
let events = vec![
    Event::new("page_view", "user_1"),
    Event::new("button_click", "user_2"),
];
client.capture_batch(events).unwrap();
```

## Backward Compatibility

Old format with full URLs still works - the SDK automatically normalizes them:

```rust
// This still works - path is automatically stripped
let options = ClientOptionsBuilder::new()
    .api_key("phc_your_api_key")
    .api_endpoint("https://eu.posthog.com/i/v0/e/")  // Gets normalized to base URL
    .build()
    .unwrap();
```

## Error Handling

The SDK provides error handling with semantic categories:

```rust
use posthog_rs::{Error, TransportError, ValidationError};

match client.capture(event).await {
    Ok(_) => println!("Event sent successfully"),
    Err(Error::Transport(TransportError::Timeout(duration))) => {
        eprintln!("Request timed out after {:?}", duration);
        // Retry logic here
    }
    Err(Error::Transport(TransportError::HttpError(401, _))) => {
        eprintln!("Invalid API key - check your configuration");
    }
    Err(e) if e.is_retryable() => {
        // Automatically handles: timeouts, 5xx errors, 429 rate limits
        tokio::time::sleep(Duration::from_secs(2)).await;
        client.capture(event).await?;
    }
    Err(e) => eprintln!("Permanent error: {}", e),
}
```

### Error Categories

- **TransportError**: Network issues (DNS, timeouts, HTTP errors, connection failures)
- **ValidationError**: Data problems (serialization, batch size, invalid timestamps)
- **InitializationError**: Configuration issues (already initialized, not initialized)

### Helper Methods

- `is_retryable()`: Returns `true` for transient errors (timeouts, 5xx, 429)
- `is_client_error()`: Returns `true` for 4xx HTTP errors

See [`examples/error_classification.rs`](examples/error_classification.rs) for comprehensive error handling patterns

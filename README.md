# PostHog Rust

Please see the main [PostHog docs](https://posthog.com/docs).

**This crate is under development**

# Quickstart

Add `posthog-rs` to your `Cargo.toml`.

```toml
[dependencies]
posthog-rs = "0.3.7"
```

```rust
let client = posthog_rs::client(env!("POSTHOG_API_KEY"));

let mut event = posthog_rs::Event::new("test", "1234");
event.insert_prop("key1", "value1").unwrap();
event.insert_prop("key2", vec!["a", "b"]).unwrap();

client.capture(event).unwrap();
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

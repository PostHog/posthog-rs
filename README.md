# PostHog Rust

Please see the main [PostHog docs](https://posthog.com/docs).

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

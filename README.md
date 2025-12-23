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

## Disabled Client

The client can be initialized without an API key, which creates a disabled client. This is useful for development environments or when you need to conditionally disable event tracking (e.g., based on user privacy settings).

```rust
// Create a disabled client (no API key).
let client = posthog_rs::client(posthog_rs::ClientOptions::default());

// Events can be captured but won't be sent to PostHog.
let event = posthog_rs::Event::new("test", "1234");
client.capture(event).unwrap(); // Returns Ok(()) without sending anything.

// Check if client is disabled.
if client.is_disabled() {
  println!("Client is disabled - events will not be sent");
}
```

# PostHog Rust

Please see the main [PostHog docs](https://posthog.com/docs).

**This crate is under development**

# Quickstart

Add `posthog-rs` to your `Cargo.toml`.

```toml
[dependencies]
posthog_rs = "0.1.0"
```

```rust
let client = posthog_rs::client(env!("POSTHOG_API_KEY"));

let mut props = HashMap::new();
props.insert("key1".to_string(), "value1".to_string());
props.insert("key2".to_string(), "value2".to_string());

let event = Event {
    event: "test".to_string(),
    properties: Properties { distinct_id: "1234".to_string(), props },
    timestamp: Some(Utc::now().naive_utc()),
};

let res = client.capture(event).unwrap();

```


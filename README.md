# PostHog Rust

Please see the main [PostHog docs](https://posthog.com/docs)

**This crate is under development**

# Quickstart

Add `posthog-rs` to your `Cargo.toml`.

```toml
[dependencies]
posthog-rs = "0.3.7"
```

```rust
let client = posthog_rs::client(env!("POSTHOG_API_KEY"));

// Capture events
let mut event = posthog_rs::Event::new("test", "1234");
event.insert_prop("key1", "value1").unwrap();
event.insert_prop("key2", vec!["a", "b"]).unwrap();

client.capture(event).unwrap();

// Check feature flags
let is_enabled = client.is_feature_enabled(
    "new-feature".to_string(),
    "user-123".to_string(),
    None,
    None,
    None,
).unwrap();

if is_enabled {
    println!("Feature is enabled!");
}
```

## Feature Flags

The SDK now supports PostHog feature flags, allowing you to control feature rollout and run A/B tests.

### Basic Usage

```rust
use posthog_rs::{client, ClientOptions, FlagValue};
use std::collections::HashMap;
use serde_json::json;

let client = client(ClientOptions::from("your-api-key"));

// Check if a feature is enabled
let is_enabled = client.is_feature_enabled(
    "feature-key".to_string(),
    "user-id".to_string(),
    None, None, None
).unwrap();

// Get feature flag value (boolean or variant)
match client.get_feature_flag(
    "feature-key".to_string(),
    "user-id".to_string(),
    None, None, None
).unwrap() {
    Some(FlagValue::Boolean(enabled)) => println!("Flag is: {}", enabled),
    Some(FlagValue::String(variant)) => println!("Variant: {}", variant),
    None => println!("Flag is disabled"),
}
```

### With Properties

```rust
// Include person properties for flag evaluation
let mut person_props = HashMap::new();
person_props.insert("plan".to_string(), json!("enterprise"));
person_props.insert("country".to_string(), json!("US"));

let flag = client.get_feature_flag(
    "premium-feature".to_string(),
    "user-id".to_string(),
    None,
    Some(person_props),
    None
).unwrap();
```

### With Groups (B2B)

```rust
// For B2B apps with group-based flags
let mut groups = HashMap::new();
groups.insert("company".to_string(), "company-123".to_string());

let mut group_props = HashMap::new();
let mut company_props = HashMap::new();
company_props.insert("size".to_string(), json!(500));
group_props.insert("company".to_string(), company_props);

let flag = client.get_feature_flag(
    "b2b-feature".to_string(),
    "user-id".to_string(),
    Some(groups),
    None,
    Some(group_props)
).unwrap();
```

### Get All Flags

```rust
// Get all feature flags for a user
let response = client.get_feature_flags(
    "user-id".to_string(),
    None, None, None
).unwrap();

for (key, value) in response.feature_flags {
    println!("Flag {}: {:?}", key, value);
}
```

### Feature Flag Payloads

```rust
// Get additional data associated with a feature flag
let payload = client.get_feature_flag_payload(
    "onboarding-flow".to_string(),
    "user-id".to_string()
).unwrap();

if let Some(data) = payload {
    println!("Payload: {}", data);
}
```

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
```

## Feature Flags

The SDK now supports PostHog feature flags, allowing you to control feature rollout and run A/B tests.

### Basic Usage

```rust
use posthog_rs::{ClientOptionsBuilder, FlagValue};
use std::collections::HashMap;
use serde_json::json;

let options = ClientOptionsBuilder::default()
    .api_key("phc_your_project_key")
    .build()
    .unwrap();

let client = posthog_rs::client(options);

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

### Local Evaluation (High Performance)

For significantly faster flag evaluation, enable local evaluation to cache flag definitions locally:

```rust
use posthog_rs::ClientOptionsBuilder;

let options = ClientOptionsBuilder::default()
    .api_key("phc_your_project_key")
    .personal_api_key("phx_your_personal_key") // Required for local evaluation
    .enable_local_evaluation(true)
    .poll_interval_seconds(30) // Update cache every 30s
    .build()
    .unwrap();

let client = posthog_rs::client(options);

// Flag evaluations now happen locally (no API calls needed)
let enabled = client.is_feature_enabled(
    "new-feature".to_string(),
    "user-123".to_string(),
    None, None, None
).unwrap();
```

**Performance:** Local evaluation is 100-1000x faster than API evaluation (~119µs vs ~125ms per request).

Get your personal API key at: https://app.posthog.com/me/settings

### Automatic Event Tracking

The SDK automatically captures `$feature_flag_called` events when you evaluate feature flags. These events include:
- Feature flag key and response value
- Deduplication per user + flag + value combination
- Rich metadata (payloads, versions, request IDs)

To disable automatic events globally:
```rust
let options = ClientOptionsBuilder::default()
    .api_key("phc_your_key")
    .send_feature_flag_events(false)
    .build()
    .unwrap();
```

# PostHog Rust

[![Crates.io](https://img.shields.io/crates/v/posthog-rs.svg)](https://crates.io/crates/posthog-rs)
[![Documentation](https://docs.rs/posthog-rs/badge.svg)](https://docs.rs/posthog-rs)

The official Rust SDK for [PostHog](https://posthog.com). See the [PostHog docs](https://posthog.com/docs) for more information.

## Features

- **Event capture** - Send events to PostHog for product analytics
- **Feature flags** - Evaluate feature flags with local or remote evaluation
- **A/B testing** - Support for multivariate flags and experiments
- **Group analytics** - Track events and flags for B2B use cases
- **Async and sync clients** - Choose based on your runtime

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
let (feature_flags, _payloads) = client.get_feature_flags(
    "user-id".to_string(),
    None, None, None
).unwrap();

for (key, value) in feature_flags {
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

### Error Handling

In production code, handle errors properly instead of using `.unwrap()`:

```rust
use posthog_rs::{client, Error, FlagValue};

let client = client("your-api-key");

// Pattern 1: Match on the error type
match client.get_feature_flag("my-flag", "user-123", None, None, None) {
    Ok(Some(FlagValue::Boolean(true))) => {
        // Flag is enabled
        enable_feature();
    }
    Ok(Some(FlagValue::String(variant))) => {
        // Multivariate flag - use the variant
        use_variant(&variant);
    }
    Ok(Some(FlagValue::Boolean(false))) | Ok(None) => {
        // Flag is disabled or doesn't exist
        use_default_behavior();
    }
    Err(Error::Connection(e)) => {
        // Network error - maybe use cached value or default
        eprintln!("Failed to fetch flag: {}", e);
        use_default_behavior();
    }
    Err(Error::InconclusiveMatch(reason)) => {
        // Local evaluation couldn't determine flag value
        // (missing properties, unknown cohort, etc.)
        eprintln!("Inconclusive evaluation: {}", reason);
        use_default_behavior();
    }
    Err(e) => {
        // Other errors
        eprintln!("Unexpected error: {}", e);
        use_default_behavior();
    }
}

// Pattern 2: Use unwrap_or for simple defaults
let is_enabled = client
    .is_feature_enabled("my-flag", "user-123", None, None, None)
    .unwrap_or(false);  // Default to disabled on error

// Pattern 3: Propagate errors with ?
fn check_feature(client: &posthog_rs::Client, user_id: &str) -> Result<bool, Error> {
    let flag = client.is_feature_enabled("premium-feature", user_id, None, None, None)?;
    Ok(flag)
}
```

## Observability

The SDK uses [tracing](https://docs.rs/tracing) for structured logging. To see logs, add a tracing subscriber to your application:

```rust
use tracing_subscriber::{fmt, EnvFilter};

// Initialize tracing (e.g., in main.rs)
tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::from_default_env())
    .init();
```

Then set the `RUST_LOG` environment variable to control log levels:

```bash
# See all posthog logs
RUST_LOG=posthog_rs=debug cargo run

# See only warnings and errors
RUST_LOG=posthog_rs=warn cargo run

# See trace-level logs for flag evaluation
RUST_LOG=posthog_rs=trace cargo run
```

Log levels:
- `error`: Connection failures, HTTP errors
- `warn`: Configuration issues, failed flag fetches
- `info`: Client initialization, poller start/stop
- `debug`: Flag evaluation results, API fallback decisions
- `trace`: Detailed cache updates, individual flag lookups

## Releasing

This repository uses [Sampo](https://github.com/bruits/sampo) for versioning, changelogs, and publishing to crates.io.

1. When making changes, include a changeset: `sampo add`
2. Create a PR with your changes and the changeset file
3. Add the `release` label and merge to `main`
4. Approve the release in Slack when prompted — this triggers version bump, crates.io publish, git tag, and GitHub Release

You can also trigger a release manually via the workflow's `workflow_dispatch` trigger (still requires pending changesets).

# Acknowledgements

Thanks to [@christos-h](https://github.com/christos-h) for building the initial version of this project.

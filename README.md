# PostHog Rust SDK

A Rust SDK for [PostHog](https://posthog.com), featuring a flexible event builder, an actor-based service for efficient event batching, and API client capabilities.

## Features

- **Event Builder Pattern**: Intuitive builder pattern for constructing PostHog events
- **Flexible Event Properties**: Use `serde_json::Value` for maximum flexibility in event property definitions
- **Actor-based Service**: Built-in actor system for efficient event batching and delivery
- **Query API Support**: Access PostHog's Query API for data analysis
- **Async/Await**: Built on Tokio for asynchronous operation
- **Error Handling**: Comprehensive error handling with proper context

## Installation

Add `posthog-rs` to your `Cargo.toml`:

```toml
[dependencies]
posthog-rs = "0.2.0"
```

## Quick Start

### Event Capture

```rust
use posthog_rs::sdk::{PostHogSDKClient, PostHogServiceActor, models::event::EventBuilder};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize PostHog client
    let client = PostHogSDKClient::new("your_api_key", "https://app.posthog.com")?;
    let actor = PostHogServiceActor::new(client);
    
    // Start the service actor
    let sender = actor.start().await;
    
    // Create and send an event
    let event = EventBuilder::new("event_name")
        .distinct_id("user_123")
        .timestamp_now()
        .properties(json!({
            "property1": "value1",
            "property2": 42,
            "nested": {
                "key": "value"
            }
        }))
        .build();
        
    sender.send(PostHogServiceMessage::Capture(event)).await?
    
    Ok(())
}
```

### Query API

```rust
use posthog_rs::api::{client::PostHogAPIClient, query::QueryRequest};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = PostHogAPIClient::new("your_api_key", "https://app.posthog.com")?;
    
    let request = QueryRequest::default().with_query(json!({
        "kind": "HogQLQuery",
        "query": "select * from events limit 10"
    }));
    
    let response = client.query("your_project_id", request).await?;
    println!("{:#?}", response);
    
    Ok(())
}
```

## Examples

Check out our [examples directory](./examples) for complete working examples:

- [Actor Microservice](./examples/actor-microservice): Using the PostHog service actor in a microservice
- [Query API](./examples/query): Executing queries against the PostHog Query API
- [Axum Server](./examples/axum-server): Integrating PostHog with an Axum web server

## API Coverage

Currently, the SDK supports:
- Event capture with the PostHog Service Actor
- Query API for data analysis

We plan to expand coverage to include:
- Feature Flags
- Annotations
- Persons and Groups
- Projects and Organizations
- And more!

## Contributing

Contributions are welcome! Feel free to:
- Open issues for bugs or feature requests
- Submit pull requests
- Improve documentation

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.


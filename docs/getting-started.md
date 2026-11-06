# Getting started

## Install

```bash
cargo add posthog-rs
cargo add tokio --features rt-multi-thread,macros
```

## Initialize and capture

Create a client with your project API key, capture an event, then shut down so queued events are sent. Without the default `async-client` feature, `client` is synchronous and the calls below drop `.await`.

```rust
use posthog_rs::{client, Event};

#[tokio::main]
async fn main() -> Result<(), posthog_rs::Error> {
    let posthog = client("<ph_project_api_key>").await;

    let mut event = Event::new("user_signed_up", "distinct_id_of_the_user");
    event.insert_prop("plan", "pro")?;
    posthog.capture(event);

    posthog.shutdown().await;
    Ok(())
}
```

`capture` queues the event and returns at once. Call `shutdown` before the process exits.

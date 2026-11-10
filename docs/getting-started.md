# Getting started

The Rust SDK runs on your server.

To send your first event, add `posthog` and `tokio`, create a client with your project token from your [project settings](https://app.posthog.com/project/settings), and call `capture`.

A background worker sends queued events, so call `shutdown` before your process exits, or the last events are lost.

## Install

```bash
cargo add posthog@1
cargo add tokio --features rt-multi-thread,macros
```

## Initialize and capture

Create a client with your project API key, capture an event, then shut down so queued events are sent. Without the default `async-client` feature, `client` is synchronous and the calls below drop `.await`.

```rust
use posthog::{client, Event};

#[tokio::main]
async fn main() -> Result<(), posthog::Error> {
    let posthog = client("<ph_project_api_key>").await;

    let mut event = Event::new("user_signed_up", "distinct_id_of_the_user");
    event.insert_prop("plan", "pro")?;
    posthog.capture(event);

    posthog.shutdown().await;
    Ok(())
}
```

`capture` queues the event and returns at once. Call `shutdown` before the process exits.

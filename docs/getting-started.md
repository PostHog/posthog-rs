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

In a web server, put the call after the server future resolves, such as after `run().await` in an actix or axum `main`.

## Read SDK logs

The SDK logs through [`tracing`](https://docs.rs/tracing) under the `posthog` target. Add a subscriber to your application to see the logs.

```rust
use tracing_subscriber::EnvFilter;

tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::from_default_env())
    .init();
```

Set `RUST_LOG` to choose the level.

```bash
RUST_LOG=posthog=debug cargo run
```

Use `posthog=warn` for warnings and errors only. Use `posthog=trace` for detailed local evaluation and cache logs.

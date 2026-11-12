# Getting started

The Rust SDK runs on your server.

To send your first event, add `posthog` and `tokio`, create a client with your project token from your [project settings](https://app.posthog.com/project/settings), and call `capture`.

A background worker sends queued events, so call `shutdown` before your process exits, or the last events are lost.

## Install

```bash
cargo add posthog
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

## Install with the wizard

Run this command in your project directory to add PostHog to your Rust project:

```bash
npx -y @posthog/wizard@latest
```

The wizard detects your framework, installs the SDK, and sets up your first events. You can also set it up by hand with the steps on this page.

## Install with Cargo.toml

Install the `posthog` crate by adding it to your `Cargo.toml`:

```toml
[dependencies]
posthog = "1"
```

Next, set up the client with your PostHog project key:

```rust
let client = posthog::client("<ph_project_token>").await;
```

Existing apps can keep the `posthog-rs` crate name. It re-exports `posthog`.

## Use the blocking client

The SDK has an async client and a blocking client. The async client is the default, and it fits most apps.

To get the blocking client, turn off the default features in your `Cargo.toml`. This also turns off TLS, so turn the `tls` feature back on to send events over HTTPS:

```toml
[dependencies]
posthog = { version = "1", default-features = false, features = ["tls"] }
```

With the blocking client, the same methods are available without `.await`. The [PostHog CLI](https://github.com/PostHog/posthog/tree/master/cli) uses it.

`capture` doesn't block in either client. It hands the event to a background worker that batches and sends it, so it returns before the network call. Call `flush()` or `shutdown()` before your program exits, or buffered events may be lost.

Feature flags work the same way without `.await`:

```rust
use posthog::EvaluateFlagsOptions;

let flags = client.evaluate_flags(
    "distinct_id_of_your_user",
    EvaluateFlagsOptions::default(),
).unwrap();

if flags.is_enabled("flag-key") {
    // Do something differently for this user
}
```

## Identify users

> **Identifying users is required.** Backend events need a `distinct_id` that matches the ID your frontend uses when calling `posthog.identify()`. Without this, backend events are orphaned. They can't be linked to frontend event captures, [session replays](https://posthog.com/docs/session-replay), [LLM traces](https://posthog.com/docs/ai-engineering), or [error tracking](https://posthog.com/docs/error-tracking).

See the guide on [identifying users](https://posthog.com/docs/getting-started/identify-users) to set this up.

## Capture custom events

You can send custom events using `capture`:

```rust
let mut event = Event::new("user_signed_up", "distinct_id_of_the_user");
client.capture(event);
```

> **Tip.** Name events in an `[object] [verb]` format. `[object]` is the entity the behavior relates to, and `[verb]` is the behavior itself. For example, `project created`, `user signed up`, or `invite sent`.

To send more information with the event, add [properties](https://posthog.com/docs/data/events#event-properties):

```rust
let mut event = Event::new("user_signed_up", "distinct_id_of_the_user");

event.insert_prop("login_type", "email").unwrap();
event.insert_prop("is_free_trial", true).unwrap();

client.capture(event);
```

To capture multiple events at once, use `capture_batch()`:

```rust
let event1 = posthog::Event::new("event 1", "distinct_id_of_user_A");
let event2 = posthog::Event::new("event 2", "distinct_id_of_user_B");

client.capture_batch(vec![event1, event2], false);
```

## Flush on shutdown

`capture` and `capture_batch` return as soon as the event is queued. A background worker batches and sends events for you, so capturing never blocks on the network. Events still queued when the process exits are lost.

Flush before your program exits:

```rust
// Send queued events now (one delivery attempt per pending batch)
client.flush().await;

// Or flush, stop the background worker, and wait for it to finish (for example, on shutdown)
client.shutdown().await;
```

In a server, call `flush` and then `shutdown` where your server future resolves. In an actix or axum `main`, that's after `run().await`:

```rust
server.run().await?;
client.flush().await;
client.shutdown().await;
```

If your app has no shutdown path today, that line is still the place. Add the calls rather than skipping the flush.

## Set person properties

The SDK captures identified events when you pass a `distinct_id` to `Event::new`. These events create [person profiles](https://posthog.com/docs/data/persons).

To set [person properties](https://posthog.com/docs/product-analytics/person-properties), include `$set` and `$set_once` properties when you capture an event.

To capture [anonymous events](https://posthog.com/docs/data/anonymous-vs-identified-events) without person profiles, use `Event::new_anon`.

## Evaluate feature flags

PostHog's [feature flags](https://posthog.com/docs/feature-flags) let you safely deploy and roll back new features, and target specific users and groups with them.

Call `client.evaluate_flags()` once for the user, then read values from the returned snapshot.

For a boolean flag, use `is_enabled`:

```rust
use posthog::EvaluateFlagsOptions;

let flags = client.evaluate_flags(
    "distinct_id_of_your_user",
    EvaluateFlagsOptions::default(),
).await.unwrap();

if flags.is_enabled("flag-key") {
    // Do something differently for this user
    // Optional: fetch the payload
    let matched_flag_payload = flags.get_flag_payload("flag-key");
}
```

For a multivariate flag, match on `get_flag`:

```rust
use posthog::{EvaluateFlagsOptions, FlagValue};

let flags = client.evaluate_flags(
    "distinct_id_of_your_user",
    EvaluateFlagsOptions::default(),
).await.unwrap();

match flags.get_flag("flag-key") {
    Some(FlagValue::String(variant)) if variant == "variant-key" => {
        // Do something differently for this user
        // Optional: fetch the payload
        let matched_flag_payload = flags.get_flag_payload("flag-key");
    }
    _ => {}
}
```

By default, `evaluate_flags()` evaluates every flag for the user. If you only need a few flags, pass `flag_keys` to request only those flags:

```rust
use posthog::EvaluateFlagsOptions;

let mut options = EvaluateFlagsOptions::default();
options.flag_keys = Some(vec!["checkout-flow".to_string(), "new-dashboard".to_string()]);

let flags = client.evaluate_flags("distinct_id_of_your_user", options).await.unwrap();
```

## Send flags with events

To break down or filter events by a flag in your [insights](https://posthog.com/docs/product-analytics/insights), include the flag's value in those events. This attributes the flag value to the event correctly.

> **Note.** You only need this for events captured with the server-side SDKs or the [API](https://posthog.com/docs/api).

You can do it in two ways. The first is to pass the same `flags` snapshot that you used for branching. This attaches the exact flag values from that evaluation and doesn't make another `/flags` request:

```rust
use posthog::{EvaluateFlagsOptions, Event};

let flags = client.evaluate_flags(
    "distinct_id_of_your_user",
    EvaluateFlagsOptions::default(),
).await.unwrap();

if flags.is_enabled("flag-key") {
    // Do something differently for this user
}

let mut event = Event::new("event_name", "distinct_id_of_your_user");
event.with_flags(&flags);
client.capture(event);
```

By default, this attaches every flag in the snapshot using `$feature/<flag-key>` properties and `$active_feature_flags`.

To attach fewer properties, pass a filtered snapshot:

```rust
// Attach only flags accessed with is_enabled() or get_flag() before this call
let mut event = Event::new("event_name", "distinct_id_of_your_user");
event.with_flags(&flags.only_accessed());
client.capture(event);

// Attach only specific flags
let mut event = Event::new("event_name", "distinct_id_of_your_user");
event.with_flags(&flags.only(&["checkout-flow", "new-dashboard"]));
client.capture(event);
```

`only_accessed()` depends on call order. If you call it before you access any flag with `is_enabled()` or `get_flag()`, no feature flag properties are attached.

The second way is to set the `$feature/feature_flag_name` property yourself, with the variant key as its value:

```rust
use posthog::Event;

let mut event = Event::new("event_name", "distinct_id_of_your_user");
event.insert_prop("$feature/feature-flag-key", "variant-key").unwrap();
client.capture(event);
```

## Track flag usage

`$feature_flag_called` events tell PostHog when a user accessed a flag, which powers [analytics and insights](https://posthog.com/docs/product-analytics/insights) on the flag. The SDK sends this event when you call `flags.is_enabled()` or `flags.get_flag()` for a flag.

The SDK deduplicates these events per `(distinct_id, flag, value)` in a local cache. If you reinitialize the PostHog client, the cache resets and `$feature_flag_called` events may be sent again. PostHog handles duplicates, so they don't affect your analytics.

`flags.get_flag_payload()` doesn't send `$feature_flag_called` events, and it doesn't count as an access for `only_accessed()`.

## Evaluate flags locally

Local evaluation caches flag definitions and evaluates flags in your process, so a flag check makes no API request.

To turn it on, you need a [secret API key](https://posthog.com/docs/feature-flags/local-evaluation#step-1-get-a-secret-api-key). Then configure the client:

```rust
use posthog::ClientOptionsBuilder;

let options = ClientOptionsBuilder::default()
    .api_key("<ph_project_token>".to_string())
    .secret_key("your-secret-api-key")
    .enable_local_evaluation(true)
    .poll_interval_seconds(30) // Optional, defaults to 30
    .build()
    .unwrap();

let client = posthog::client(options).await;
```

The SDK fetches flag definitions when the client starts and refreshes them in the background.

> **Note.** Local evaluation needs every person property, group, and group property that the flag's release conditions use. PostHog can't fetch these for you without a server request.

## Capture panics

`capture_exception` and `capture_exception_with` capture errors you handle. To also capture unhandled panics, enable `capture_panics` and initialize the global client with `init_global`.

For the full setup, see the [Rust error tracking installation docs](https://posthog.com/docs/error-tracking/installation/rust).

## Read SDK logs

The SDK uses [`tracing`](https://docs.rs/tracing) for structured logging. Add a tracing subscriber to your application to see SDK logs:

```rust
use tracing_subscriber::{fmt, EnvFilter};

tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::from_default_env())
    .init();
```

Set `RUST_LOG` to control the log level:

```bash
RUST_LOG=posthog=debug cargo run
```

Use `posthog=warn` for warnings and errors only, or `posthog=trace` for detailed local evaluation and cache logs.

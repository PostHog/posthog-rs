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

## LLM Analytics (Generations)

Capture AI generation events with canonical PostHog properties. Works with async and blocking clients.

```rust
// async example
let client = posthog_rs::client(env!("POSTHOG_API_KEY")).await;

let gen = posthog_rs::GenerationBuilder::new()
    .distinct_id("user_123")
    .provider("google")
    .model("gemini-2.0-flash-exp")
    .input(serde_json::json!({"messages": [{"role":"user","content":"Write a haiku"}]}))?
    .output(serde_json::json!({"choices":[{"message":{"content":"Rusty code whispers\nmetrics flow like mountain streams\ninsights bloom in graphs"}}]}))?
    .input_tokens(45)
    .output_tokens(28)
    .total_tokens(73)
    .latency_ms(842)
    .cost_usd(0.00091);

client.capture_generation(gen).await?;
```

This sends a `$ai_generation` event with properties such as `$ai_provider`, `$ai_model`, `$ai_input`, `$ai_output`, `$ai_input_tokens`, `$ai_output_tokens`, `$ai_total_tokens`, `$ai_latency_ms`, and `$ai_total_cost_usd`.

### Traces and Spans

```rust
let trace = posthog_rs::TraceBuilder::new()
    .distinct_id("user_123")
    .trace_id("trace_1")
    .name("chat_session")
    .start_timer()
    .finish()?; // builds `$ai_trace`

let span = posthog_rs::SpanBuilder::new()
    .distinct_id("user_123")
    .trace_id("trace_1")
    .span_id("span_1")
    .parent_span_id("root")
    .name("gemini.generate")
    .start_timer()
    .finish()?; // builds `$ai_span`
```

### Embeddings

```rust
let embedding = posthog_rs::EmbeddingBuilder::new()
    .distinct_id("user_123")
    .provider("google")
    .model("text-embedding-004")
    .vector_dims(768)
    .vector_count(1)
    .input_tokens(42)
    .start_timer()
    .finish()?; // builds `$ai_embedding`
```

### Privacy Modes

```rust
use posthog_rs::PrivacyMode;

let gen = posthog_rs::GenerationBuilder::new()
    .distinct_id("user_123")
    .input(serde_json::json!({"secret":"text"}))?
    .input_privacy(PrivacyMode::Redacted) // will send "[REDACTED]" in $ai_input
    .build_event()?;
```

## Rig integration (optional)

Enable the `rig-integration` feature and map Rig events to PostHog:

```toml
[dependencies]
posthog-rs = { version = "0.3.7", features = ["rig-integration"] }
```

```rust
use posthog_rs::integrations::rig::{generation_to_event, RigGeneration};

let rig_gen = RigGeneration {
    distinct_id: "user_123",
    provider: Some("google"),
    model: Some("gemini-2.0-pro"),
    input: Some(serde_json::json!({"messages": []})),
    output: None,
    latency_ms: Some(250),
    input_tokens: Some(100),
    output_tokens: Some(50),
    total_tokens: Some(150),
    request_id: Some("req_1"),
    trace_id: Some("trace_1"),
};

let event = generation_to_event(rig_gen)?;
client.capture(event)?;
```

Observer pattern (when Rig exposes hooks):

```rust
// blocking
use posthog_rs::RigPosthogObserver;
let observer = RigPosthogObserver::new(client);
// inside your Rig callback
observer.on_generation(rig_gen)?;

// async
use posthog_rs::AsyncRigPosthogObserver;
let observer = AsyncRigPosthogObserver::new(client);
observer.on_generation(rig_gen).await?;
```

Note: If Rig does not yet provide hook traits to subscribe to lifecycle events, call the observer methods from your own instrumentation around Rig requests. If Rig introduces official hooks, this crate can implement a dedicated listener integrating automatically.

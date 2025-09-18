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

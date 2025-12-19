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

## Publishing

This crate is automatically published to crates.io when a PR is merged to `main` with the `publish` label. The publish workflow:

To publish a new version:
1. Update the version in `Cargo.toml`
2. Create a PR with your changes
3. Add the `publish` label to the PR
4. Merge the PR to trigger automatic publishing

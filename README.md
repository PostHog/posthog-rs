# PostHog Rust

Please see the main [PostHog docs](https://posthog.com/docs).

**This crate is under development**

# Quickstart

Add `posthog-rs` to your `Cargo.toml`.

```toml
[dependencies]
posthog-rs = "0.2.0" # for sync client
async-posthog = "0.2.0" # for async client
```

## Events

Capture events with `capture`.

```rust
let client = crate::client(env!("POSTHOG_API_KEY"));

let mut event = Event::new("test", "1234");
event.insert_prop("key1", "value1").unwrap();
event.insert_prop("key2", vec!["a", "b"]).unwrap();

client.capture(event).unwrap();
```

## Groups

[Group analytics](https://posthog.com/docs/product-analytics/group-analytics) are supported.

### Identifying Groups

Groups can be created with [`group_identify`](https://posthog.com/docs/product-analytics/group-analytics#how-to-create-groups).

```rust
let client = crate::client(env!("POSTHOG_API_KEY"));

let mut event = GroupIdentify::new("organisation", "some_id");
event.insert_prop("status", "active").unwrap();

client.group_identify(event).unwrap();

```

### Associating Events with a Group

```rust
let client = crate::client(env!("POSTHOG_API_KEY"));

let mut event = Event::new("test", "1234");

// Optionally associate this event with a group (in this case,
// a "company" group type with key "company_id_123").
event.insert_group("company", "company_id_123");

client.capture(event).unwrap();
```

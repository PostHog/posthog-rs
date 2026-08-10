# PostHog Rust

[![Crates.io](https://img.shields.io/crates/v/posthog-rs.svg)](https://crates.io/crates/posthog-rs)
[![Documentation](https://docs.rs/posthog-rs/badge.svg)](https://docs.rs/posthog-rs)

The official Rust SDK for [PostHog](https://posthog.com). See the main [PostHog docs](https://posthog.com/docs) for more information.

SDK usage examples and code snippets live in the official documentation so they stay up to date.

## Documentation

- [Rust library docs](https://posthog.com/docs/libraries/rust)

## Capture pipeline

The V1 capture pipeline is enabled by default. To continue using the legacy V0
pipeline, disable the default features and explicitly enable the other features
your application needs:

```toml
posthog-rs = { version = "1", default-features = false, features = ["async-client", "error-tracking"] }
```

This example retains the default asynchronous client and error tracking while
opting out of `capture-v1`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for local setup and test instructions.

## Releasing

See [RELEASING.md](RELEASING.md).

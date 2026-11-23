# `posthog-rs` compatibility crate

The PostHog Rust SDK is now published as [`posthog`](https://crates.io/crates/posthog). This package preserves the former `posthog-rs` package name by forwarding its features and re-exporting the canonical crate.

Existing applications can continue using `posthog_rs`. New applications should depend on and import `posthog` directly.

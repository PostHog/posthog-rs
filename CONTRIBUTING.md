# Contributing

Thanks for your interest in improving the PostHog Rust SDK.

## Prerequisites

- Rust `1.78.0` or newer (see `Cargo.toml`)

## Development commands

From the repository root:

```bash
cargo build --verbose
cargo test --verbose
cargo test --verbose --features e2e-test --no-default-features
cargo fmt -- --check
cargo clippy -- -D warnings
```

## Running examples

See [examples/README.md](examples/README.md) for the available example programs and the environment variables they use.

## Pull requests

Please make sure the relevant build, test, formatting, and clippy checks pass before opening a PR.

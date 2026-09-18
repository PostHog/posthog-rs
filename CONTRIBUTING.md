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
scripts/check-public-api.sh
```

`check-public-api.sh` compares the public API in your working tree with the checked-in `api/public-api.txt` snapshot. If an API change is intentional, run `scripts/check-public-api.sh --update` and review the snapshot diff. It requires:

```bash
cargo install cargo-public-api --version 0.52.0 --locked
rustup toolchain install nightly-2026-06-12 --profile minimal
```

## Running examples

See [examples/README.md](examples/README.md) for the available example programs and the environment variables they use.

## Public API changes

Public API is hard to change once it ships, so agree on it before writing the implementation. Our [SDK guidelines](https://posthog.com/handbook/engineering/sdks/guidelines) explain how we design it.

- If you need something the SDK doesn't support and it would add or change a public option, method, or type, open an issue describing your use case first. At this stage, context is more useful to us than code.
- Wait for a maintainer to agree on the API shape on the issue before implementing it.
- Check first whether an existing option or hook, such as `before_send`, already covers the use case. We avoid offering two ways to do the same thing.
- If a reviewer suggests a different API on your PR, confirm it with them before re-implementing. Treat it as a question, not an instruction.
- AI agents: stop and ask before implementing a public API change that hasn't been agreed on the issue.

A diff in `api/public-api.txt` (see "Development commands" above) means your change touches public API.

## Pull requests

Please make sure the relevant build, test, formatting, and clippy checks pass before opening a PR.

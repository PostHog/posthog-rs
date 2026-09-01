#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all -- --check
cargo clippy -- -D warnings
scripts/check-public-api.sh
cargo check --examples
cargo test --workspace
cargo test --no-default-features
cargo test --no-default-features --features error-tracking
cargo publish --workspace --dry-run --locked

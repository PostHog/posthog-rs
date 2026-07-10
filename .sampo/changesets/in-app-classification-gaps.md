---
cargo/posthog-rs: patch
---

Default in-app frame classification now covers three gaps that left dependency and runtime frames marked as app code:

- Cargo registry/git checkout paths are recognized for cargo homes not named `~/.cargo` — `/usr/local/cargo/registry/...` in the official Rust Docker images, and crates.io registry layouts (`registry/src/index.crates.io-<hash>/`) under any relocated `CARGO_HOME`.
- Crate-denylist matching strips trailing generic arguments first, so DWARF-derived names like `poll_future<tokio::runtime::...>` no longer produce a garbage leading segment (`poll_future<tokio`) that bypasses the check.
- Fileless thread/process bootstrap symbols (`__clone`, `start_thread`, `_start`, the C `main` shim, and friends) are classified as not in-app. Other bare symbols — `#[no_mangle]` exports, C code linked into the binary — keep their previous in-app classification.

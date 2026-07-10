---
cargo/posthog-rs: patch
---

Default in-app frame classification now covers three gaps that left dependency and runtime frames marked as app code:

- Cargo registry/git checkout paths are recognized under any `CARGO_HOME`, not just `~/.cargo` — e.g. `/usr/local/cargo/registry/...` in the official Rust Docker images.
- Crate-denylist matching strips trailing generic arguments first, so DWARF-derived names like `poll_future<tokio::runtime::...>` no longer produce a garbage leading segment (`poll_future<tokio`) that bypasses the check.
- Bare bootstrap/foreign symbols with no source file and no `::` path (`__clone`, `start_thread`, `main`) are classified as not in-app.

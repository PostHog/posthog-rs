---
cargo/posthog-rs: minor
---

TLS backend selection is now a feature choice. `reqwest/rustls` was previously listed unconditionally on the dependency line, so every consumer compiled and linked `aws-lc-rs` -> `aws-lc-sys` with no way to opt out. Two features now control this: `tls` (enabled by default) keeps rustls with reqwest's built-in aws-lc-rs provider, and `tls-no-provider` links rustls with no crypto provider so an application can supply its own.

Applications that already install a rustls `CryptoProvider` gained nothing from the linked one — rustls resolves the process-level provider before consulting crate features, so an app calling `ring::default_provider().install_default()` already used ring at runtime while still building and linking aws-lc-rs. `tls-no-provider` drops `aws-lc-sys` from the tree entirely, removing the C/assembly build dependency (C toolchain, cmake, nasm on some targets) that complicates cross-compilation, musl targets, and minimal build containers.

With `tls-no-provider` the application MUST install a process-level provider before constructing a client; reqwest builds its TLS connector at construction time and rustls panics if no provider is available. Cargo features are additive, so `tls-no-provider` only takes effect when nothing in the dependency graph enables `tls`.

Default users are unaffected — `tls` is in `default` and behavior is unchanged. **Breaking for `default-features = false` consumers:** TLS is no longer implied, so add `tls` (or `tls-no-provider`) explicitly. Without either, the build succeeds but HTTPS requests fail at runtime with `invalid URL, scheme is not http`.

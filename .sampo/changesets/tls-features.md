---
cargo/posthog-rs: major
---

TLS is now a Cargo feature. `default-features = false` no longer enables TLS: add `tls` for reqwest's default Rustls provider, or `tls-no-provider` when the application installs a process-level Rustls `CryptoProvider`. With `tls-no-provider`, constructing a client before a provider is installed panics.

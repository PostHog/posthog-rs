---
cargo/posthog-rs: major
---

Make TLS backend selection explicit. Default users continue to use Rustls with reqwest's built-in provider through the new `tls` default feature. Applications can instead enable `tls-no-provider` and install their own process-level Rustls `CryptoProvider`, avoiding the `aws-lc-sys` native build dependency when no other dependency enables `tls`.

This is breaking for `default-features = false` consumers: TLS is no longer enabled implicitly, so HTTPS clients must enable either `tls` or `tls-no-provider`.

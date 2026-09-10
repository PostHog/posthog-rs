#![cfg(feature = "tls-no-provider")]
//! Bring-your-own-provider TLS: with `tls-no-provider` (and without `tls`)
//! the SDK links no rustls crypto provider, so the application must install
//! a process-level provider before constructing a client. Reqwest builds its
//! TLS connector at client construction time and rustls panics if no provider
//! is available, so successfully constructing a client is the regression check.

use posthog_rs::{ClientOptions, ClientOptionsBuilder};

fn install_ring_provider() {
    // Ignore the result: `install_default` fails if a process-level provider
    // is already installed.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn options() -> ClientOptions {
    ClientOptionsBuilder::default()
        .api_key("phc_test_token".to_string())
        .host("https://eu.i.posthog.com".to_string())
        .build()
        .unwrap()
}

#[cfg(not(feature = "async-client"))]
#[test]
fn blocking_client_builds_with_installed_provider() {
    install_ring_provider();
    let _client = posthog_rs::client(options());
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn async_client_builds_with_installed_provider() {
    install_ring_provider();
    let _client = posthog_rs::client(options()).await;
}

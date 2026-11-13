#![cfg(feature = "tls-no-provider")]
//! Bring-your-own-provider TLS: with `tls-no-provider` (and without `tls`)
//! the SDK links no rustls crypto provider, so the application must install
//! a process-level provider before constructing a client. Reqwest builds its
//! TLS connector at client construction time and rustls panics if no provider
//! is available, so successfully constructing a client is the regression check.

use posthog::{ClientOptions, ClientOptionsBuilder};

fn install_ring_provider() {
    // Ignore the result: `install_default` fails if a process-level provider
    // is already installed.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(feature = "stateful")]
fn stateful_config() -> posthog::stateful::Config {
    let mut config = posthog::stateful::Config::new("phc_test_token");
    config.preload_feature_flags = false;
    config.host = "https://127.0.0.1:1".into();
    config.persistence = posthog::stateful::Persistence::Memory;
    config
}

#[cfg(feature = "stateful")]
#[test]
fn stateful_blocking_initialization_acknowledges_installed_provider() {
    install_ring_provider();
    let client = posthog::stateful::Client::init_blocking(stateful_config());
    assert!(!client.is_disabled());
    assert_eq!(
        client.shutdown_blocking(None).delivery,
        posthog::stateful::DeliveryOutcome::Stopped
    );
}

#[cfg(all(feature = "stateful", feature = "async-client"))]
#[test]
fn stateful_async_initialization_acknowledges_provider_without_application_runtime() {
    install_ring_provider();
    futures::executor::block_on(async {
        let client = posthog::stateful::Client::init(stateful_config()).await;
        assert!(!client.is_disabled());
        assert_eq!(
            client.shutdown(None).await.delivery,
            posthog::stateful::DeliveryOutcome::Stopped
        );
    });
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
    let _client = posthog::client(options());
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn async_client_builds_with_installed_provider() {
    install_ring_provider();
    let _client = posthog::client(options()).await;
}

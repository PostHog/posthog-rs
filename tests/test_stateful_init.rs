#![cfg(feature = "stateful")]

use std::sync::{Arc, Mutex};

use posthog::stateful::{Client, Config, DeliveryOutcome, Persistence, PersistenceOutcome};
use posthog::PostHogError;

#[test]
fn invalid_token_returns_the_same_client_type_with_noop_methods() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let output = messages.clone();
    let mut config = Config::new("");
    config.on_error(move |error| {
        if let PostHogError::Initialization(failure) = error {
            output.lock().unwrap().push(failure.message().to_owned());
        }
    });
    let client: Client = Client::init_blocking(config);
    assert!(client.is_disabled());
    assert!(client.get_distinct_id().is_empty());
    assert!(client.get_anonymous_id().is_empty());
    assert_eq!(client.flush_blocking().delivery, DeliveryOutcome::Disabled);
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Disabled
    );
    assert_eq!(messages.lock().unwrap().len(), 1);
}

#[test]
fn enabled_memory_client_exposes_identity_and_drain_reports() {
    let mut config = Config::new("phc_test");
    config.preload_feature_flags = false;
    config.persistence = Persistence::Memory;
    let client: Client = Client::init_blocking(config);
    assert!(!client.is_disabled());
    assert!(!client.get_distinct_id().is_empty());
    assert_eq!(client.get_distinct_id(), client.get_anonymous_id());
    let report = client.flush_blocking();
    assert_eq!(report.state, PersistenceOutcome::MemoryOnly);
    assert_eq!(report.delivery, DeliveryOutcome::Attempted);
    assert_eq!(
        client.shutdown_blocking(None).delivery,
        DeliveryOutcome::Stopped
    );
}

#[cfg(feature = "async-client")]
#[tokio::test(flavor = "current_thread")]
async fn async_initialization_returns_a_ready_client_on_an_application_runtime() {
    let mut config = Config::new("phc_test");
    config.preload_feature_flags = false;
    config.persistence = Persistence::Memory;
    config.bootstrap_anonymous_id = Some("seeded-anonymous-id".into());
    let client: Client = Client::init(config).await;
    assert!(!client.is_disabled());
    assert_eq!(client.get_distinct_id(), "seeded-anonymous-id");
    assert_eq!(
        client.shutdown_blocking(None).delivery,
        DeliveryOutcome::Stopped
    );
}

#[test]
fn preload_setting_only_controls_startup_requests() {
    for enabled in [false, true] {
        let server = httpmock::MockServer::start();
        let flags = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/flags/")
                .query_param("v", "2");
            then.status(200)
                .json_body(serde_json::json!({"featureFlags":{}}));
        });
        let mut config = Config::new("phc_test");
        assert!(config.preload_feature_flags);
        if !enabled {
            config.preload_feature_flags = false;
        }
        config.host = server.base_url();
        config.persistence = Persistence::Memory;
        config.before_send(|_| None);
        let client = Client::init_blocking(config);
        client.flush_blocking();
        flags.assert_calls(usize::from(enabled));
        client.reload_feature_flags();
        client.flush_blocking();
        flags.assert_calls(usize::from(enabled) + 1);
        client.identify("person");
        client.flush_blocking();
        flags.assert_calls(usize::from(enabled) + 2);
    }
}

#[test]
fn disabled_or_invalid_clients_do_not_start_preload() {
    let server = httpmock::MockServer::start();
    let flags = server.mock(|when, then| {
        when.method(httpmock::Method::POST).path("/flags/");
        then.status(200)
            .json_body(serde_json::json!({"featureFlags":{}}));
    });
    for disabled in [false, true] {
        let mut config = Config::new(if disabled { "phc_test" } else { "" });
        assert!(config.preload_feature_flags);
        config.host = server.base_url();
        config.persistence = Persistence::Memory;
        config.disabled = disabled;
        let client = Client::init_blocking(config);
        assert!(client.is_disabled());
        client.shutdown_blocking(None);
    }
    flags.assert_calls(0);
}

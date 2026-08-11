#![cfg(not(feature = "async-client"))]

use httpmock::prelude::*;
use serde_json::json;

/// Ported from the removed V0 suite: a client with an absent or blank API key
/// is disabled and makes no network calls at all. Distinct from the
/// `disabled(true)` path — this exercises `ClientOptions::sanitize`'s
/// blank-key handling, including whitespace-only keys.
#[test]
fn test_client_with_empty_api_key_is_noop() {
    for api_key in [None, Some(" \n\t ")] {
        assert_disabled_client_is_noop(api_key);
    }
}

fn assert_disabled_client_is_noop(api_key: Option<&str>) {
    let server = MockServer::start();

    let capture_mock = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    });
    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/").query_param("v", "2");
        then.status(200).json_body(json!({
            "featureFlags": {},
            "featureFlagPayloads": {}
        }));
    });

    let mut options_builder = posthog::ClientOptionsBuilder::default();
    if let Some(api_key) = api_key {
        options_builder.api_key(api_key.to_string());
    }
    let options = options_builder.host(server.base_url()).build().unwrap();
    assert!(options.is_disabled());

    let client = posthog::client(options);
    let event = posthog::Event::new("test_event", "user1");

    client.capture(event.clone());
    client.capture_batch(vec![event], false);

    let flags = client
        .evaluate_flags("test-user", posthog::EvaluateFlagsOptions::default())
        .unwrap();
    assert!(flags.keys().is_empty());

    capture_mock.assert_hits(0);
    flags_mock.assert_hits(0);
}

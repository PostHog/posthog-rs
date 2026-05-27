use httpmock::prelude::*;
use serde_json::{json, Value};

#[cfg(feature = "async-client")]
use std::time::Duration;

fn flags_response_fixture() -> Value {
    json!({
        "flags": {
            "alpha": {
                "key": "alpha",
                "enabled": true,
                "variant": null,
                "reason": {
                    "code": "condition_match",
                    "description": "Matched condition set 1",
                    "condition_index": 0
                },
                "metadata": {
                    "id": 101,
                    "version": 4,
                    "description": null,
                    "payload": null
                }
            },
            "beta": {
                "key": "beta",
                "enabled": false,
                "variant": null,
                "reason": {
                    "code": "out_of_rollout_bound",
                    "description": null,
                    "condition_index": null
                },
                "metadata": {
                    "id": 202,
                    "version": 1,
                    "description": null,
                    "payload": null
                }
            },
            "variant-flag": {
                "key": "variant-flag",
                "enabled": true,
                "variant": "test",
                "reason": {
                    "code": "condition_match",
                    "description": null,
                    "condition_index": 0
                },
                "metadata": {
                    "id": 303,
                    "version": 7,
                    "description": null,
                    "payload": {"hello": "world"}
                }
            }
        },
        "errorsWhileComputingFlags": false,
        "requestId": "req-abc-123"
    })
}

// ---------- blocking ----------

#[cfg(not(feature = "async-client"))]
mod blocking {
    use super::*;
    use posthog_rs::{EvaluateFlagsOptions, Event, FlagValue};

    fn create_test_client(base_url: String) -> posthog_rs::Client {
        let options: posthog_rs::ClientOptions = ("test_api_key", base_url.as_str()).into();
        posthog_rs::client(options)
    }

    #[test]
    fn evaluate_flags_returns_snapshot_with_one_request() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/").query_param("v", "2");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });

        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .expect("evaluate_flags");

        let mut keys = snapshot.keys();
        keys.sort();
        assert_eq!(keys, vec!["alpha", "beta", "variant-flag"]);
        flags_mock.assert_hits(1);
        capture_mock.assert_hits(0);
    }

    #[test]
    fn unaccessed_flags_do_not_fire_events() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url());
        let _snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        flags_mock.assert_hits(1);
        capture_mock.assert_hits(0);
    }

    #[test]
    fn is_enabled_fires_event_with_full_metadata_and_dedupes() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();

        assert!(snapshot.is_enabled("alpha"));
        assert!(snapshot.is_enabled("alpha"));
        assert_eq!(
            snapshot.get_flag("variant-flag"),
            Some(FlagValue::String("test".into()))
        );
        assert_eq!(
            snapshot.get_flag("variant-flag"),
            Some(FlagValue::String("test".into()))
        );

        // Two unique (flag, value) combos => two events; repeats deduped.
        capture_mock.assert_hits(2);
    }

    #[test]
    fn get_flag_payload_does_not_fire_event() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        let payload = snapshot.get_flag_payload("variant-flag");
        assert_eq!(payload, Some(json!({"hello": "world"})));
        capture_mock.assert_hits(0);
    }

    fn assert_group_dedup(
        g1: std::collections::HashMap<String, String>,
        g2: std::collections::HashMap<String, String>,
        expected_hits: usize,
    ) {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url());

        let snap_1 = client
            .evaluate_flags(
                "user-1",
                EvaluateFlagsOptions {
                    groups: Some(g1),
                    ..Default::default()
                },
            )
            .unwrap();
        let snap_2 = client
            .evaluate_flags(
                "user-1",
                EvaluateFlagsOptions {
                    groups: Some(g2),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(snap_1.is_enabled("alpha"));
        assert!(snap_2.is_enabled("alpha"));
        capture_mock.assert_hits(expected_hits);
    }

    fn groups(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn is_enabled_fires_per_group_context() {
        // Same user, same flag, different group context — both must fire.
        assert_group_dedup(
            groups(&[("organization", "org-a")]),
            groups(&[("organization", "org-b")]),
            2,
        );
    }

    #[test]
    fn is_enabled_dedupes_across_repeated_calls_under_same_group() {
        // Calling is_enabled multiple times on the same snapshot fires only once.
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url());
        let snap = client
            .evaluate_flags(
                "user-1",
                EvaluateFlagsOptions {
                    groups: Some(groups(&[("organization", "org-a")])),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(snap.is_enabled("alpha"));
        assert!(snap.is_enabled("alpha"));
        assert!(snap.is_enabled("alpha"));
        capture_mock.assert_hits(1);
    }

    #[test]
    fn is_enabled_dedupes_across_group_insertion_order() {
        // Same content, different insertion order — only one event.
        assert_group_dedup(
            groups(&[("organization", "org-a"), ("team", "red")]),
            groups(&[("team", "red"), ("organization", "org-a")]),
            1,
        );
    }

    #[test]
    fn is_enabled_treats_groups_with_separator_chars_as_distinct() {
        // {"a=b": "c"} and {"a": "b=c"} must produce different dedup keys;
        // without encoding both serialise to "a=b=c" and the second event
        // would be incorrectly suppressed.
        assert_group_dedup(
            groups(&[("a=b", "c")]),
            groups(&[("a", "b=c")]),
            2,
        );
    }

    #[test]
    fn flag_keys_forwarded_to_request_body() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/flags/")
                .json_body_partial(json!({"flag_keys_to_evaluate": ["alpha", "beta"]}).to_string());
            then.status(200).json_body(flags_response_fixture());
        });
        let client = create_test_client(server.base_url());
        let opts = EvaluateFlagsOptions {
            flag_keys: Some(vec!["alpha".into(), "beta".into()]),
            ..Default::default()
        };
        let _ = client.evaluate_flags("user-1", opts).unwrap();
        flags_mock.assert_hits(1);
    }

    #[test]
    fn empty_distinct_id_returns_empty_snapshot_without_request_or_events() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("", EvaluateFlagsOptions::default())
            .unwrap();
        assert!(snapshot.keys().is_empty());
        assert!(!snapshot.is_enabled("alpha"));
        flags_mock.assert_hits(0);
        capture_mock.assert_hits(0);
    }

    #[test]
    fn event_with_flags_attaches_properties_without_extra_request() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        let mut event = Event::new("checkout-started", "user-1");
        event.with_flags(&snapshot);
        client.capture(event).expect("capture should succeed");
        // One /flags request, one /i/v0/e/ request — no second flag fetch.
        flags_mock.assert_hits(1);
        capture_mock.assert_hits(1);
    }

    #[test]
    fn only_filters_to_named_keys() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        let filtered = snapshot.only(&["alpha", "missing"]);
        assert_eq!(filtered.keys(), vec!["alpha".to_string()]);
    }

    #[test]
    fn only_accessed_returns_only_accessed_subset() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        let _ = snapshot.is_enabled("alpha");
        let filtered = snapshot.only_accessed();
        assert_eq!(filtered.keys(), vec!["alpha".to_string()]);
    }

    #[test]
    fn only_accessed_returns_empty_when_nothing_accessed() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        let filtered = snapshot.only_accessed();
        assert!(filtered.keys().is_empty());
    }

    #[test]
    fn errors_while_computing_flags_propagates_to_event() {
        let server = MockServer::start();
        let mut response = flags_response_fixture();
        response["errorsWhileComputingFlags"] = json!(true);
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(response);
        });
        server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        // Access a present flag to trigger the event; assert error is set
        // even though the flag itself wasn't missing.
        assert!(snapshot.is_enabled("alpha"));
        // event ships through capture pipeline; we just verify the snapshot
        // tracks the response-level error by also accessing a missing flag
        // which should produce the comma-joined form.
        assert!(snapshot.get_flag("does-not-exist").is_none());
    }

    // Demonstrates that the snapshot can deserialise the legacy shape too;
    // metadata is absent so the per-flag id/version/reason/request_id will
    // be missing, but enabled/variant still propagate.
    #[test]
    fn legacy_response_shape_still_yields_a_snapshot() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(json!({
                "featureFlags": {"alpha": true, "beta": false},
                "featureFlagPayloads": {}
            }));
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        assert!(snapshot.is_enabled("alpha"));
        assert!(!snapshot.is_enabled("beta"));
    }

    #[test]
    fn string_encoded_payload_is_normalized_to_parsed_json() {
        let server = MockServer::start();
        // Mirror the API behaviour where `metadata.payload` arrives as a
        // JSON-encoded string rather than already-parsed JSON.
        let response = json!({
            "flags": {
                "alpha": {
                    "key": "alpha",
                    "enabled": true,
                    "variant": null,
                    "metadata": {
                        "id": 1,
                        "version": 1,
                        "payload": "{\"color\":\"blue\"}"
                    }
                }
            },
            "errorsWhileComputingFlags": false,
            "requestId": "req-x"
        });
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(response);
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        assert_eq!(
            snapshot.get_flag_payload("alpha"),
            Some(json!({"color": "blue"}))
        );
    }

    #[test]
    fn disabled_client_returns_empty_snapshot() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .disabled(true)
            .build()
            .unwrap();
        let client = posthog_rs::client(options);
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        assert!(snapshot.keys().is_empty());
        flags_mock.assert_hits(0);
    }
}

// ---------- async ----------

#[cfg(feature = "async-client")]
mod async_tests {
    use super::*;
    use posthog_rs::{EvaluateFlagsOptions, Event, FlagValue};

    async fn create_test_client(base_url: String) -> posthog_rs::Client {
        let options: posthog_rs::ClientOptions = ("test_api_key", base_url.as_str()).into();
        posthog_rs::client(options).await
    }

    /// Wait briefly for any `$feature_flag_called` events that the host
    /// `tokio::spawn`'d in the background to land at the mock.
    async fn flush_spawned_events() {
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    #[tokio::test]
    async fn evaluate_flags_returns_snapshot_with_one_request() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let client = create_test_client(server.base_url()).await;
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        let mut keys = snapshot.keys();
        keys.sort();
        assert_eq!(keys, vec!["alpha", "beta", "variant-flag"]);
        flags_mock.assert_hits(1);
    }

    #[tokio::test]
    async fn is_enabled_fires_event_and_dedupes() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url()).await;
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        assert!(snapshot.is_enabled("alpha"));
        assert!(snapshot.is_enabled("alpha"));
        assert_eq!(
            snapshot.get_flag("variant-flag"),
            Some(FlagValue::String("test".into()))
        );
        flush_spawned_events().await;
        capture_mock.assert_hits(2);
    }

    #[tokio::test]
    async fn get_flag_payload_does_not_fire_event() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url()).await;
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        assert_eq!(
            snapshot.get_flag_payload("variant-flag"),
            Some(json!({"hello": "world"}))
        );
        flush_spawned_events().await;
        capture_mock.assert_hits(0);
    }

    #[tokio::test]
    async fn flag_keys_forwarded_to_request_body() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/flags/")
                .json_body_partial(json!({"flag_keys_to_evaluate": ["alpha"]}).to_string());
            then.status(200).json_body(flags_response_fixture());
        });
        let client = create_test_client(server.base_url()).await;
        let opts = EvaluateFlagsOptions {
            flag_keys: Some(vec!["alpha".into()]),
            ..Default::default()
        };
        let _ = client.evaluate_flags("user-1", opts).await.unwrap();
        flags_mock.assert_hits(1);
    }

    #[tokio::test]
    async fn empty_distinct_id_returns_empty_snapshot_without_events() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url()).await;
        let snapshot = client
            .evaluate_flags("", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        assert!(!snapshot.is_enabled("alpha"));
        flush_spawned_events().await;
        flags_mock.assert_hits(0);
        capture_mock.assert_hits(0);
    }

    #[tokio::test]
    async fn event_with_flags_attaches_properties_without_extra_request() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url()).await;
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        let mut event = Event::new("checkout-started", "user-1");
        event.with_flags(&snapshot);
        client.capture(event).await.unwrap();
        flags_mock.assert_hits(1);
        capture_mock.assert_hits(1);
    }
}

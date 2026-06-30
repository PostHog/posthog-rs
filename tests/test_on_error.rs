//! End-to-end coverage for the `on_error` hook on the feature-flags and
//! local-evaluation poller surfaces (the capture surface is covered by the
//! transport unit tests). Each test drives the public client against an
//! httpmock server and asserts the hook observes the failure.

use httpmock::prelude::*;
use posthog_rs::PostHogError;
use serde_json::json;
use std::sync::{Arc, Mutex};

/// A single local-eval flag that resolves without extra properties, so a
/// degraded `evaluate_flags` still produces a non-empty snapshot.
fn definitions_body() -> serde_json::Value {
    json!({
        "flags": [
            {
                "key": "feature-a",
                "active": true,
                "filters": {
                    "groups": [
                        { "properties": [], "rollout_percentage": 100.0, "variant": null }
                    ],
                    "multivariate": null,
                    "payloads": {}
                }
            }
        ],
        "group_type_mapping": {},
        "cohorts": {}
    })
}

type FlagsRecord = (Option<u16>, Option<String>, bool, Option<String>, bool);
type FlagsSink = Arc<Mutex<Vec<FlagsRecord>>>;
type LocalEvalSink = Arc<Mutex<Vec<Option<u16>>>>;

/// Records every `FeatureFlags` failure the hook sees.
fn flags_sink() -> (FlagsSink, impl Fn(&PostHogError<'_>) + Send + 'static) {
    let recorded: FlagsSink = Arc::new(Mutex::new(Vec::new()));
    let sink = recorded.clone();
    let hook = move |failure: &PostHogError<'_>| {
        if let PostHogError::FeatureFlags(f) = failure {
            sink.lock().unwrap_or_else(|p| p.into_inner()).push((
                f.status(),
                f.distinct_id().map(str::to_string),
                f.endpoint().contains("/flags/"),
                f.body().map(str::to_string),
                f.error().to_string().contains("500"),
            ));
        }
    };
    (recorded, hook)
}

/// Records the HTTP status of every `LocalEvaluation` failure the hook sees.
fn local_eval_sink() -> (LocalEvalSink, impl Fn(&PostHogError<'_>) + Send + 'static) {
    let recorded: LocalEvalSink = Arc::new(Mutex::new(Vec::new()));
    let sink = recorded.clone();
    let hook = move |failure: &PostHogError<'_>| {
        if let PostHogError::LocalEvaluation(e) = failure {
            sink.lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(e.status());
        }
    };
    (recorded, hook)
}

#[cfg(not(feature = "async-client"))]
mod blocking {
    use super::*;
    use posthog_rs::EvaluateFlagsOptions;

    #[test]
    fn flags_failure_reports_status_endpoint_and_body() {
        let server = MockServer::start();
        let flags = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(500).body("boom");
        });
        let (recorded, hook) = flags_sink();
        let client = posthog_rs::client(
            posthog_rs::ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                .on_error(hook)
                .build()
                .unwrap(),
        );

        let err = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .expect_err("a 500 with no local results propagates");
        assert!(err.to_string().contains("500"));
        flags.assert_hits(1);

        let recorded = recorded.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(recorded.len(), 1, "exactly one flags failure expected");
        let (status, distinct_id, endpoint_ok, body, error_has_500) = &recorded[0];
        assert_eq!(*status, Some(500));
        assert_eq!(distinct_id.as_deref(), Some("user-1"));
        assert!(*endpoint_ok);
        assert_eq!(body.as_deref(), Some("boom"));
        assert!(error_has_500);
    }

    #[test]
    fn flags_failure_fires_even_when_degrading_to_local_results() {
        // Local evaluation covers the flag, so a failed remote `/flags` degrades
        // to a local-only snapshot (Ok) rather than erroring — the hook must
        // still observe the remote failure.
        let server = MockServer::start();
        let _defs = server.mock(|when, then| {
            when.method(GET).path("/flags/definitions/");
            then.status(200).json_body(definitions_body());
        });
        let flags = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(500).body("boom");
        });
        let (recorded, hook) = flags_sink();
        let client = posthog_rs::client(
            posthog_rs::ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                .personal_api_key("phx_test".to_string())
                .enable_local_evaluation(true)
                .poll_interval_seconds(3600)
                .on_error(hook)
                .build()
                .unwrap(),
        );

        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .expect("degrades to a local-only snapshot");
        assert!(snapshot.keys().contains(&"feature-a".to_string()));
        flags.assert_hits(1);

        let recorded = recorded.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(recorded.len(), 1, "degrade path still reports once");
        assert_eq!(recorded[0].0, Some(500));
    }

    #[test]
    fn local_eval_poller_reports_initial_load_failure() {
        let server = MockServer::start();
        let defs = server.mock(|when, then| {
            when.method(GET).path("/flags/definitions/");
            then.status(401).body("unauthorized");
        });
        let (recorded, hook) = local_eval_sink();
        let _client = posthog_rs::client(
            posthog_rs::ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                .personal_api_key("phx_test".to_string())
                .enable_local_evaluation(true)
                .poll_interval_seconds(3600)
                .on_error(hook)
                .build()
                .unwrap(),
        );

        defs.assert_hits(1);
        let recorded = recorded.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(recorded.len(), 1, "initial poll failure reported once");
        assert_eq!(recorded[0], Some(401));
    }
}

#[cfg(feature = "async-client")]
mod async_tests {
    use super::*;
    use posthog_rs::EvaluateFlagsOptions;
    use std::time::Duration;

    #[tokio::test]
    async fn flags_failure_reports_status_endpoint_and_body() {
        let server = MockServer::start();
        let flags = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(500).body("boom");
        });
        let (recorded, hook) = flags_sink();
        let client = posthog_rs::client(
            posthog_rs::ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                .on_error(hook)
                .build()
                .unwrap(),
        )
        .await;

        let err = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .expect_err("a 500 with no local results propagates");
        assert!(err.to_string().contains("500"));
        flags.assert_hits(1);

        let recorded = recorded.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(recorded.len(), 1, "exactly one flags failure expected");
        let (status, distinct_id, endpoint_ok, body, error_has_500) = &recorded[0];
        assert_eq!(*status, Some(500));
        assert_eq!(distinct_id.as_deref(), Some("user-1"));
        assert!(*endpoint_ok);
        assert_eq!(body.as_deref(), Some("boom"));
        assert!(error_has_500);
    }

    #[tokio::test]
    async fn flags_failure_fires_even_when_degrading_to_local_results() {
        let server = MockServer::start();
        let _defs = server.mock(|when, then| {
            when.method(GET).path("/flags/definitions/");
            then.status(200).json_body(definitions_body());
        });
        let flags = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(500).body("boom");
        });
        let (recorded, hook) = flags_sink();
        let client = posthog_rs::client(
            posthog_rs::ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                .personal_api_key("phx_test".to_string())
                .enable_local_evaluation(true)
                .poll_interval_seconds(3600)
                .on_error(hook)
                .build()
                .unwrap(),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .expect("degrades to a local-only snapshot");
        assert!(snapshot.keys().contains(&"feature-a".to_string()));
        flags.assert_hits(1);

        let recorded = recorded.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(recorded.len(), 1, "degrade path still reports once");
        assert_eq!(recorded[0].0, Some(500));
    }

    #[tokio::test]
    async fn local_eval_poller_reports_initial_load_failure() {
        let server = MockServer::start();
        let defs = server.mock(|when, then| {
            when.method(GET).path("/flags/definitions/");
            then.status(401).body("unauthorized");
        });
        let (recorded, hook) = local_eval_sink();
        let _client = posthog_rs::client(
            posthog_rs::ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                .personal_api_key("phx_test".to_string())
                .enable_local_evaluation(true)
                .poll_interval_seconds(3600)
                .on_error(hook)
                .build()
                .unwrap(),
        )
        .await;

        defs.assert_hits(1);
        let recorded = recorded.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(recorded.len(), 1, "initial poll failure reported once");
        assert_eq!(recorded[0], Some(401));
    }
}

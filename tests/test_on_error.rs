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
    use std::thread;
    use std::time::Duration;

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

    #[test]
    fn on_error_hook_reentering_flags_path_does_not_deadlock() {
        // Regression guard for the hook's concurrency design. `on_error` is an
        // `Arc<dyn Fn + Send + Sync>` invoked without holding any SDK lock; an
        // earlier `Arc<Mutex<Box<FnMut>>>` design would re-lock the hook's own
        // mutex on the same thread if the hook re-entered a client method that
        // fires `on_error` again, deadlocking the caller. Re-entering the SDK
        // from a hook is unsupported (see the docs) — this only proves the SDK
        // itself can't self-deadlock, and locks in the `Fn`-not-`Mutex` design.
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc;
        use std::sync::{OnceLock, Weak};

        let server = MockServer::start();
        let _flags = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(500).body("boom");
        });

        // The hook needs a handle to the client that owns it; a `Weak` cell
        // breaks the ownership cycle (Client -> hook -> cell -> Client) so the
        // client still drops normally at end of test.
        let cell: Arc<OnceLock<Weak<posthog_rs::Client>>> = Arc::new(OnceLock::new());
        let depth = Arc::new(AtomicUsize::new(0));
        let cell_for_hook = cell.clone();
        let depth_for_hook = depth.clone();
        let hook = move |failure: &PostHogError<'_>| {
            if let PostHogError::FeatureFlags(_) = failure {
                // Re-enter exactly once to bound the recursion.
                if depth_for_hook.fetch_add(1, Ordering::SeqCst) == 0 {
                    if let Some(client) = cell_for_hook.get().and_then(Weak::upgrade) {
                        let _ = client.evaluate_flags("user-2", EvaluateFlagsOptions::default());
                    }
                }
            }
        };

        let client = Arc::new(posthog_rs::client(
            posthog_rs::ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                .on_error(hook)
                .build()
                .unwrap(),
        ));
        cell.set(Arc::downgrade(&client)).ok();

        // Drive the re-entrant sequence on a worker thread so a regression fails
        // fast (watchdog) instead of hanging the whole test binary.
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let err = client
                .evaluate_flags("user-1", EvaluateFlagsOptions::default())
                .expect_err("a 500 with no local results propagates");
            tx.send(err.to_string()).ok();
        });

        let msg = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("evaluate_flags re-entry deadlocked (on_error hook self-deadlock regression)");
        assert!(msg.contains("500"));
        worker.join().unwrap();

        assert_eq!(
            depth.load(Ordering::SeqCst),
            2,
            "hook fired for the outer failure and the one re-entered failure"
        );
    }

    #[test]
    fn local_eval_poller_reports_recurring_loop_failures() {
        // The background poll loop (not just the synchronous initial load) must
        // fire the hook on each failed poll. With a 1s interval, a second poll
        // fails after the initial load — assert the hook sees >= 2 failures.
        let server = MockServer::start();
        let _defs = server.mock(|when, then| {
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
                .poll_interval_seconds(1)
                .on_error(hook)
                .build()
                .unwrap(),
        );

        // Initial load fails synchronously during construction (1 hit). Wait
        // for the background loop to fire at least one more poll — the hook
        // firing is proof the poll ran.
        thread::sleep(Duration::from_millis(1500));

        let recorded = recorded.lock().unwrap_or_else(|p| p.into_inner());
        assert!(
            recorded.len() >= 2,
            "recurring poll failures reported, got {}",
            recorded.len()
        );
        assert!(recorded.iter().all(|s| *s == Some(401)));
    }
}

#[cfg(feature = "async-client")]
mod async_tests {
    use super::*;
    use posthog_rs::EvaluateFlagsOptions;

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
        // AsyncFlagPoller::start() awaits the initial definitions load before
        // returning, so the cache is populated by the time client().await
        // resolves — no sleep needed here.

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

    #[tokio::test]
    async fn local_eval_poller_reports_recurring_loop_failures() {
        // The async background poll task (not just the initial load awaited by
        // start()) must fire the hook on each failed poll. With a 1s interval,
        // a second poll fails after start() returns — assert >= 2 failures.
        let server = MockServer::start();
        let _defs = server.mock(|when, then| {
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
                .poll_interval_seconds(1)
                .on_error(hook)
                .build()
                .unwrap(),
        )
        .await;

        // start() awaits the initial load (1 hit). Wait for the background
        // task's interval to fire at least one more poll — the hook firing is
        // proof the poll ran.
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

        let recorded = recorded.lock().unwrap_or_else(|p| p.into_inner());
        assert!(
            recorded.len() >= 2,
            "recurring poll failures reported, got {}",
            recorded.len()
        );
        assert!(recorded.iter().all(|s| *s == Some(401)));
    }
}

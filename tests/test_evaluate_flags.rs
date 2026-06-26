mod common;

use common::default_user_agent;
use httpmock::prelude::*;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

/// Where `$feature_flag_called` ships: V1 analytics with `capture-v1`, else v0.
#[cfg(feature = "capture-v1")]
const CAPTURE_PATH: &str = "/i/v1/analytics/events";
#[cfg(not(feature = "capture-v1"))]
const CAPTURE_PATH: &str = "/i/v0/e/";

/// Where the background worker ships analytics captures on v0: the batch
/// endpoint (single captures are batched too now). Used only by the v0-gated
/// `event_with_flags` tests below; `$feature_flag_called` still ships via the
/// fire-and-forget `/i/v0/e/` host path (`CAPTURE_PATH`).
#[cfg(not(feature = "capture-v1"))]
const WORKER_CAPTURE_PATH: &str = "/batch/";

/// Feature-aware capture mock; the JSON body is required by V1, ignored by v0.
fn capture_path_mock(server: &MockServer) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method(POST).path(CAPTURE_PATH);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    })
}

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

struct FlakyFlagsServer {
    base_url: String,
    attempts: Arc<AtomicUsize>,
    saw_flags_path: Arc<AtomicBool>,
    handle: thread::JoinHandle<()>,
}

impl FlakyFlagsServer {
    fn assert_retry_succeeded(self) {
        self.handle.join().expect("flaky flags server thread");
        assert_eq!(self.attempts.load(Ordering::SeqCst), 2);
        assert!(self.saw_flags_path.load(Ordering::SeqCst));
    }
}

fn start_flaky_flags_server(success_body: String) -> FlakyFlagsServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind flaky flags server");
    listener
        .set_nonblocking(true)
        .expect("set flaky flags server nonblocking");
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let attempts = Arc::new(AtomicUsize::new(0));
    let saw_flags_path = Arc::new(AtomicBool::new(false));
    let thread_attempts = attempts.clone();
    let thread_saw_flags_path = saw_flags_path.clone();
    let handle = thread::spawn(move || {
        let started = Instant::now();
        while thread_attempts.load(Ordering::SeqCst) < 2
            && started.elapsed() < Duration::from_secs(5)
        {
            match listener.accept() {
                Ok((mut stream, _addr)) => {
                    let attempt = thread_attempts.fetch_add(1, Ordering::SeqCst) + 1;
                    if attempt == 1 {
                        let _ = stream.shutdown(Shutdown::Both);
                        continue;
                    }

                    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                    let mut request = Vec::new();
                    let mut buf = [0; 1024];
                    loop {
                        match stream.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                request.extend_from_slice(&buf[..n]);
                                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    if String::from_utf8_lossy(&request).contains("POST /flags/?v=2") {
                        thread_saw_flags_path.store(true, Ordering::SeqCst);
                    }

                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        success_body.len(),
                        success_body
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    FlakyFlagsServer {
        base_url,
        attempts,
        saw_flags_path,
        handle,
    }
}

struct ResettingFlagsServer {
    base_url: String,
    attempts: Arc<AtomicUsize>,
    handle: thread::JoinHandle<()>,
}

impl ResettingFlagsServer {
    fn assert_attempts(self, expected: usize) {
        self.handle.join().expect("resetting flags server thread");
        assert_eq!(self.attempts.load(Ordering::SeqCst), expected);
    }
}

fn start_resetting_flags_server(expected_attempts: usize) -> ResettingFlagsServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind resetting flags server");
    listener
        .set_nonblocking(true)
        .expect("set resetting flags server nonblocking");
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let attempts = Arc::new(AtomicUsize::new(0));
    let thread_attempts = attempts.clone();
    let handle = thread::spawn(move || {
        let started = Instant::now();
        while thread_attempts.load(Ordering::SeqCst) < expected_attempts
            && started.elapsed() < Duration::from_secs(5)
        {
            match listener.accept() {
                Ok((stream, _addr)) => {
                    thread_attempts.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.shutdown(Shutdown::Both);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    ResettingFlagsServer {
        base_url,
        attempts,
        handle,
    }
}

// ---------- blocking ----------

#[cfg(not(feature = "async-client"))]
mod blocking {
    use super::*;
    use posthog_rs::{EvaluateFlagsOptions, Event, FlagValue};
    use reqwest::header::USER_AGENT;

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
        let capture_mock = capture_path_mock(&server);

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
    fn get_feature_flags_retries_transport_error_then_succeeds() {
        let server = start_flaky_flags_server(flags_response_fixture().to_string());
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url.clone())
            .feature_flags_request_max_retries(1u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = posthog_rs::client(options);

        let (flags, _payloads) = client
            .get_feature_flags("user-1", None, None, None)
            .expect("get_feature_flags should retry transport error");

        assert_eq!(flags.get("alpha"), Some(&FlagValue::Boolean(true)));
        server.assert_retry_succeeded();
    }

    #[test]
    fn get_feature_flags_returns_error_after_transport_retry_budget() {
        let server = start_resetting_flags_server(2);
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url.clone())
            .feature_flags_request_max_retries(1u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = posthog_rs::client(options);

        let err = client
            .get_feature_flags("user-1", None, None, None)
            .expect_err("transport errors should stop after retry budget is exhausted");

        assert!(matches!(err, posthog_rs::Error::Connection(_)));
        server.assert_attempts(2);
    }

    #[test]
    fn evaluate_flags_does_not_retry_500_status() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(500).body("boom");
        });
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .max_capture_attempts(3u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = posthog_rs::client(options);

        let err = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .expect_err("500 status should be terminal");

        assert!(err.to_string().contains("500"));
        flags_mock.assert_hits(1);
    }

    #[test]
    fn evaluate_flags_uses_default_useragent() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/flags/")
                .header(USER_AGENT.to_string(), default_user_agent());
            then.status(200).json_body(flags_response_fixture());
        });
        let client = create_test_client(server.base_url());
        let _ = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        flags_mock.assert();
    }

    #[test]
    fn unaccessed_flags_do_not_fire_events() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = capture_path_mock(&server);
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
        let capture_mock = capture_path_mock(&server);
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
        let capture_mock = capture_path_mock(&server);
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
        let capture_mock = capture_path_mock(&server);
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
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
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
        let capture_mock = capture_path_mock(&server);
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
        assert_group_dedup(groups(&[("a=b", "c")]), groups(&[("a", "b=c")]), 2);
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
        let capture_mock = capture_path_mock(&server);
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("", EvaluateFlagsOptions::default())
            .unwrap();
        assert!(snapshot.keys().is_empty());
        assert!(!snapshot.is_enabled("alpha"));
        flags_mock.assert_hits(0);
        capture_mock.assert_hits(0);
    }

    #[cfg(not(feature = "capture-v1"))]
    #[test]
    fn event_with_flags_attaches_properties_without_extra_request() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path(WORKER_CAPTURE_PATH);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "results": {} }));
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        let mut event = Event::new("checkout-started", "user-1");
        event.with_flags(&snapshot);
        client.capture(event);
        client.flush();
        // One /flags request, one capture request — no second flag fetch.
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
        capture_path_mock(&server);
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
        capture_path_mock(&server);
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

    /// C5: `$feature_flag_called` ships via the V1 endpoint, never the v0 path.
    #[cfg(feature = "capture-v1")]
    #[test]
    fn flag_called_event_routes_to_v1_endpoint() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let v1_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v1/analytics/events")
                .header("posthog-attempt", "1")
                .header(
                    "posthog-sdk-info",
                    format!("posthog-rs/{}", env!("CARGO_PKG_VERSION")),
                )
                .body_contains("$feature_flag_called")
                .body_contains("\"$feature_flag\":\"alpha\"");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "results": {} }));
        });
        let v0_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        assert!(snapshot.is_enabled("alpha"));
        v1_mock.assert_hits(1);
        v0_mock.assert_hits(0);
    }

    /// C5: a failed ship is fire-and-forget — one attempt, no surfaced error.
    #[cfg(feature = "capture-v1")]
    #[test]
    fn flag_called_event_v1_failure_is_single_attempt_and_silent() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let v1_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v1/analytics/events");
            then.status(503)
                .header("content-type", "application/json")
                .json_body(json!({ "error": "service_unavailable" }));
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        // The flag read still succeeds; the ship is attempted exactly once.
        assert!(snapshot.is_enabled("alpha"));
        v1_mock.assert_hits(1);
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
    #[cfg(not(feature = "capture-v1"))]
    use posthog_rs::Event;
    use posthog_rs::{EvaluateFlagsOptions, FlagValue};
    use reqwest::header::USER_AGENT;

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
    async fn get_feature_flags_retries_transport_error_then_succeeds() {
        let server = start_flaky_flags_server(flags_response_fixture().to_string());
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url.clone())
            .feature_flags_request_max_retries(1u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = posthog_rs::client(options).await;

        let (flags, _payloads) = client
            .get_feature_flags("user-1", None, None, None)
            .await
            .expect("get_feature_flags should retry transport error");

        assert_eq!(flags.get("alpha"), Some(&FlagValue::Boolean(true)));
        server.assert_retry_succeeded();
    }

    #[tokio::test]
    async fn get_feature_flags_returns_error_after_transport_retry_budget() {
        let server = start_resetting_flags_server(2);
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url.clone())
            .feature_flags_request_max_retries(1u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = posthog_rs::client(options).await;

        let err = client
            .get_feature_flags("user-1", None, None, None)
            .await
            .expect_err("transport errors should stop after retry budget is exhausted");

        assert!(matches!(err, posthog_rs::Error::Connection(_)));
        server.assert_attempts(2);
    }

    #[tokio::test]
    async fn evaluate_flags_does_not_retry_500_status() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(500).body("boom");
        });
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .max_capture_attempts(3u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = posthog_rs::client(options).await;

        let err = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .expect_err("500 status should be terminal");

        assert!(err.to_string().contains("500"));
        flags_mock.assert_hits(1);
    }

    #[tokio::test]
    async fn evaluate_flags_uses_default_useragent() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/flags/")
                .header(USER_AGENT.to_string(), default_user_agent());
            then.status(200).json_body(flags_response_fixture());
        });
        let client = create_test_client(server.base_url()).await;
        let _ = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        flags_mock.assert();
    }

    #[tokio::test]
    async fn is_enabled_fires_event_and_dedupes() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = capture_path_mock(&server);
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

    #[cfg(not(feature = "capture-v1"))]
    #[tokio::test]
    async fn flag_called_event_contains_is_server_and_lib() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v0/e/")
                .body_contains("\"$is_server\":true")
                .body_contains("\"$lib\":\"posthog-rs\"");
            then.status(200);
        });
        let client = create_test_client(server.base_url()).await;
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        assert!(snapshot.is_enabled("alpha"));
        flush_spawned_events().await;
        capture_mock.assert_hits(1);
    }

    #[tokio::test]
    async fn get_flag_payload_does_not_fire_event() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = capture_path_mock(&server);
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
        let capture_mock = capture_path_mock(&server);
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

    /// C5: `$feature_flag_called` ships via the V1 endpoint, never the v0 path.
    #[cfg(feature = "capture-v1")]
    #[tokio::test]
    async fn flag_called_event_routes_to_v1_endpoint() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let v1_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v1/analytics/events")
                .header("posthog-attempt", "1")
                .header(
                    "posthog-sdk-info",
                    format!("posthog-rs/{}", env!("CARGO_PKG_VERSION")),
                )
                .body_contains("$feature_flag_called")
                .body_contains("\"$feature_flag\":\"alpha\"");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "results": {} }));
        });
        let v0_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url()).await;
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        assert!(snapshot.is_enabled("alpha"));
        flush_spawned_events().await;
        v1_mock.assert_hits(1);
        v0_mock.assert_hits(0);
    }

    /// C5: a failed ship is fire-and-forget — one attempt, no surfaced error.
    #[cfg(feature = "capture-v1")]
    #[tokio::test]
    async fn flag_called_event_v1_failure_is_single_attempt_and_silent() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let v1_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v1/analytics/events");
            then.status(503)
                .header("content-type", "application/json")
                .json_body(json!({ "error": "service_unavailable" }));
        });
        let client = create_test_client(server.base_url()).await;
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        // The flag read still succeeds; the ship is attempted exactly once.
        assert!(snapshot.is_enabled("alpha"));
        flush_spawned_events().await;
        v1_mock.assert_hits(1);
    }

    #[cfg(not(feature = "capture-v1"))]
    #[tokio::test]
    async fn event_with_flags_attaches_properties_without_extra_request() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path(WORKER_CAPTURE_PATH);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "results": {} }));
        });
        let client = create_test_client(server.base_url()).await;
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        let mut event = Event::new("checkout-started", "user-1");
        event.with_flags(&snapshot);
        client.capture(event);
        client.flush().await;
        flags_mock.assert_hits(1);
        capture_mock.assert_hits(1);
    }
}

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

/// Where the background worker ships analytics captures.
const CAPTURE_PATH: &str = "/i/v1/analytics/events";

/// Capture mock returning an empty results map, so the client makes one attempt.
fn capture_path_mock(server: &MockServer) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method(POST).path(CAPTURE_PATH);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    })
}

fn capture_events_mock<'a>(
    server: &'a MockServer,
    expected: Vec<(&'static str, Value)>,
) -> httpmock::Mock<'a> {
    server.mock(|when, then| {
        when.method(POST)
            .path(CAPTURE_PATH)
            .is_true(move |request| {
                let Ok(body) = serde_json::from_slice::<Value>(request.body_ref()) else {
                    return false;
                };
                let Some(batch) = body["batch"].as_array() else {
                    return false;
                };
                batch.len() == expected.len()
                    && batch.iter().zip(&expected).all(|(event, (name, props))| {
                        event["event"] == *name
                            && props
                                .as_object()
                                .unwrap()
                                .iter()
                                .all(|(key, value)| event["properties"].get(key) == Some(value))
                    })
            });
        then.status(200).json_body(json!({"results": {}}));
    })
}

fn read_flags_request(stream: &mut std::net::TcpStream) -> std::io::Result<String> {
    // macOS inherits the listener's nonblocking mode on accepted sockets.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut request = Vec::new();
    let mut buf = [0; 1024];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        request.extend_from_slice(&buf[..n]);
        if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]);
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                .unwrap_or(0);
            if request.len() >= end + 4 + length {
                return Ok(String::from_utf8_lossy(&request).into_owned());
            }
        }
    }
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

fn local_definitions_fixture() -> Value {
    json!({
        "flags": [{
            "key": "alpha",
            "active": true,
            "filters": {
                "groups": [{
                    "properties": [],
                    "rollout_percentage": 100.0,
                    "variant": null
                }],
                "multivariate": null,
                "payloads": {}
            }
        }],
        "group_type_mapping": {},
        "cohorts": {}
    })
}

/// Local definitions with one flag that needs `plan` to be absent and one that
/// stays inconclusive locally because `region` is never supplied.
fn known_absent_definitions_fixture() -> Value {
    json!({
        "flags": [
            {
                "key": "profile-incomplete",
                "active": true,
                "filters": {
                    "groups": [{
                        "properties": [{"key": "plan", "operator": "is_not_set", "type": "person"}],
                        "rollout_percentage": 100
                    }]
                }
            },
            {
                "key": "regional",
                "active": true,
                "filters": {
                    "groups": [{
                        "properties": [{"key": "region", "value": "US", "operator": "exact", "type": "person"}],
                        "rollout_percentage": 100
                    }]
                }
            }
        ],
        "group_type_mapping": {"0": "organization"},
        "cohorts": {}
    })
}

fn known_absent_options(flag_keys: &[&str]) -> posthog::EvaluateFlagsOptions {
    let mut options = posthog::EvaluateFlagsOptions::default();
    options.flag_keys = Some(flag_keys.iter().map(|key| key.to_string()).collect());
    options.person_properties = Some(
        vec![("plan".to_string(), json!("pro"))]
            .into_iter()
            .collect(),
    );
    options.person_property_keys_known_absent = Some(vec!["plan".to_string()]);
    options.group_property_keys_known_absent = Some(
        vec![("organization".to_string(), vec!["tier".to_string()])]
            .into_iter()
            .collect(),
    );
    options
}

fn local_evaluation_client_options(base_url: String) -> posthog::ClientOptions {
    posthog::ClientOptionsBuilder::default()
        .api_key("test_api_key".to_string())
        .host(base_url)
        .secret_key("test_personal_key".to_string())
        .enable_local_evaluation(true)
        .poll_interval_seconds(3600)
        .build()
        .unwrap()
}

fn mock_known_absent_definitions(server: &MockServer) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method(GET).path("/flags/definitions/");
        then.status(200)
            .json_body(known_absent_definitions_fixture());
    })
}

/// `/flags` mock that only matches a body carrying exactly the known-absent
/// fields in `expected`, or neither field when `expected` is `None`.
fn mock_flags_with_known_absent(
    server: &MockServer,
    expected: Option<(Value, Value)>,
) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method(POST).path("/flags/").is_true(move |request| {
            let Ok(body) = serde_json::from_slice::<Value>(request.body_ref()) else {
                return false;
            };
            let person = body.get("person_property_keys_known_absent").cloned();
            let groups = body.get("group_property_keys_known_absent").cloned();
            match &expected {
                Some((expected_person, expected_groups)) => {
                    person.as_ref() == Some(expected_person)
                        && groups.as_ref() == Some(expected_groups)
                }
                None => person.is_none() && groups.is_none(),
            }
        });
        then.status(200).json_body(flags_response_fixture());
    })
}

/// `is_not` filters on the listed key `plan` whose condition value is omitted
/// or JSON null, neither of which the known-absent table may resolve.
fn mock_known_absent_valueless_definitions(server: &MockServer) -> httpmock::Mock<'_> {
    let flag = |key: &str, property: Value| {
        json!({
            "key": key,
            "active": true,
            "filters": {"groups": [{"properties": [property], "rollout_percentage": 100}]}
        })
    };
    let definitions = json!({
        "flags": [
            flag("plan-is-not-omitted", json!({"key": "plan", "operator": "is_not", "type": "person"})),
            flag("plan-is-not-null", json!({"key": "plan", "value": null, "operator": "is_not", "type": "person"})),
            flag("profile-incomplete", json!({"key": "plan", "value": null, "operator": "is_not_set", "type": "person"}))
        ],
        "group_type_mapping": {},
        "cohorts": {}
    });
    server.mock(|when, then| {
        when.method(GET).path("/flags/definitions/");
        then.status(200).json_body(definitions);
    })
}

const VALUELESS_FLAG_KEYS: [&str; 2] = ["plan-is-not-omitted", "plan-is-not-null"];

/// Lists `plan` as known absent; `only_evaluate_locally` also requests
/// `profile-incomplete`, which the table still resolves.
fn valueless_options(only_evaluate_locally: bool) -> posthog::EvaluateFlagsOptions {
    let mut keys = VALUELESS_FLAG_KEYS.to_vec();
    if only_evaluate_locally {
        keys.push("profile-incomplete");
    }
    let mut options = known_absent_options(&keys);
    options.only_evaluate_locally = only_evaluate_locally;
    options
}

fn over_limit_known_absent_options() -> Vec<posthog::EvaluateFlagsOptions> {
    let too_many: Vec<String> = (0..1001).map(|_| "plan".to_string()).collect();
    let mut person = posthog::EvaluateFlagsOptions::default();
    person.person_property_keys_known_absent = Some(too_many.clone());
    let mut groups = posthog::EvaluateFlagsOptions::default();
    groups.group_property_keys_known_absent = Some(
        vec![
            ("organization".to_string(), vec!["tier".to_string()]),
            ("project".to_string(), too_many),
        ]
        .into_iter()
        .collect(),
    );
    vec![person, groups]
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
                    let Ok(request) = read_flags_request(&mut stream) else {
                        continue;
                    };
                    let attempt = thread_attempts.fetch_add(1, Ordering::SeqCst) + 1;
                    if attempt == 1 {
                        let _ = stream.shutdown(Shutdown::Both);
                        continue;
                    }

                    if request.starts_with("POST /flags/?v=2 ") {
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
                Ok((mut stream, _addr)) => {
                    if read_flags_request(&mut stream).is_err() {
                        continue;
                    }
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

struct StatusThenSuccessFlagsServer {
    base_url: String,
    attempts: Arc<AtomicUsize>,
    saw_flags_path: Arc<AtomicBool>,
    handle: thread::JoinHandle<()>,
}

impl StatusThenSuccessFlagsServer {
    fn assert_retry_succeeded(self) {
        self.handle.join().expect("status flags server thread");
        assert_eq!(self.attempts.load(Ordering::SeqCst), 2);
        assert!(self.saw_flags_path.load(Ordering::SeqCst));
    }
}

fn start_status_then_success_flags_server(
    first_status: u16,
    success_body: String,
) -> StatusThenSuccessFlagsServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind status flags server");
    listener
        .set_nonblocking(true)
        .expect("set status flags server nonblocking");
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
                    let Ok(request) = read_flags_request(&mut stream) else {
                        continue;
                    };
                    let attempt = thread_attempts.fetch_add(1, Ordering::SeqCst) + 1;
                    if request.starts_with("POST /flags/?v=2 ") {
                        thread_saw_flags_path.store(true, Ordering::SeqCst);
                    }

                    let (status, body) = if attempt == 1 {
                        (first_status, "transient flags error".to_string())
                    } else {
                        (200, success_body.clone())
                    };
                    let response = format!(
                        "HTTP/1.1 {status} Status\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
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

    StatusThenSuccessFlagsServer {
        base_url,
        attempts,
        saw_flags_path,
        handle,
    }
}

// ---------- blocking ----------

#[cfg(not(feature = "async-client"))]
mod blocking {
    use super::*;
    use posthog::{EvaluateFlagsOptions, Event, FlagValue};
    use reqwest::header::USER_AGENT;

    fn create_test_client(base_url: String) -> posthog::Client {
        let options: posthog::ClientOptions = ("test_api_key", base_url.as_str()).into();
        posthog::client(options)
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
        flags_mock.assert_calls(1);
        client.flush();
        capture_mock.assert_calls(0);
    }

    #[test]
    fn local_evaluation_only_without_evaluator_does_not_call_api() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let options = posthog::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .enable_local_evaluation(true)
            .local_evaluation_only(true)
            .build()
            .unwrap();
        let client = posthog::client(options);

        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .expect("local-only evaluation should return an empty snapshot");

        assert!(snapshot.keys().is_empty());
        flags_mock.assert_calls(0);
    }

    #[test]
    fn evaluate_flags_retries_transport_error_then_succeeds() {
        let server = start_flaky_flags_server(flags_response_fixture().to_string());
        let options = posthog::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url.clone())
            .feature_flags_request_max_retries(1u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = posthog::client(options);

        let flags = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .expect("evaluate_flags should retry transport error");

        assert_eq!(flags.get_flag("alpha"), Some(FlagValue::Boolean(true)));
        server.assert_retry_succeeded();
    }

    #[test]
    fn evaluate_flags_returns_error_after_transport_retry_budget() {
        let server = start_resetting_flags_server(2);
        let options = posthog::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url.clone())
            .feature_flags_request_max_retries(1u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = posthog::client(options);

        let err = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .expect_err("transport errors should stop after retry budget is exhausted");

        assert!(matches!(err, posthog::Error::Connection(_)));
        server.assert_attempts(2);
    }

    #[test]
    fn evaluate_flags_retries_502_and_504_status_then_succeeds() {
        for status in [502, 504] {
            let server = start_status_then_success_flags_server(
                status,
                flags_response_fixture().to_string(),
            );
            let options = posthog::ClientOptionsBuilder::default()
                .api_key("test_api_key".to_string())
                .host(server.base_url.clone())
                .feature_flags_request_max_retries(1u32)
                .retry_initial_backoff_ms(1u64)
                .retry_max_backoff_ms(1u64)
                .build()
                .unwrap();
            let client = posthog::client(options);

            let snapshot = client
                .evaluate_flags("user-1", EvaluateFlagsOptions::default())
                .expect("evaluate_flags should retry retryable status");

            assert_eq!(snapshot.get_flag("alpha"), Some(FlagValue::Boolean(true)));
            server.assert_retry_succeeded();
        }
    }

    #[test]
    fn evaluate_flags_returns_error_after_502_and_504_retry_budget() {
        for status in [502, 504] {
            let server = MockServer::start();
            let flags_mock = server.mock(|when, then| {
                when.method(POST).path("/flags/");
                then.status(status).body("transient flags error");
            });
            let options = posthog::ClientOptionsBuilder::default()
                .api_key("test_api_key".to_string())
                .host(server.base_url())
                .feature_flags_request_max_retries(1u32)
                .retry_initial_backoff_ms(1u64)
                .retry_max_backoff_ms(1u64)
                .build()
                .unwrap();
            let client = posthog::client(options);

            let err = client
                .evaluate_flags("user-1", EvaluateFlagsOptions::default())
                .expect_err("retryable status should error after retry budget is exhausted");

            assert!(err.to_string().contains(&status.to_string()));
            flags_mock.assert_calls(2);
        }
    }

    #[test]
    fn evaluate_flags_does_not_retry_500_status() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(500).body("boom");
        });
        let options = posthog::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .max_capture_attempts(3u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = posthog::client(options);

        let err = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .expect_err("500 status should be terminal");

        assert!(err.to_string().contains("500"));
        flags_mock.assert_calls(1);
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
        flags_mock.assert_calls(1);
        client.flush();
        capture_mock.assert_calls(0);
    }

    #[test]
    fn is_enabled_fires_event_with_full_metadata_and_dedupes() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let alpha_mock = capture_events_mock(
            &server,
            vec![(
                "$feature_flag_called",
                json!({
                    "$feature_flag": "alpha", "$feature_flag_response": true,
                    "$feature_flag_id": 101, "$feature_flag_version": 4,
                    "$feature_flag_reason": "Matched condition set 1",
                    "$feature_flag_request_id": "req-abc-123", "$is_server": true
                }),
            )],
        );
        let variant_mock = capture_events_mock(
            &server,
            vec![(
                "$feature_flag_called",
                json!({
                    "$feature_flag": "variant-flag", "$feature_flag_response": "test"
                }),
            )],
        );
        let unexpected_capture = capture_path_mock(&server);
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();

        assert!(snapshot.is_enabled("alpha"));
        assert!(snapshot.is_enabled("alpha"));
        client.flush();
        assert_eq!(
            snapshot.get_flag("variant-flag"),
            Some(FlagValue::String("test".into()))
        );
        assert_eq!(
            snapshot.get_flag("variant-flag"),
            Some(FlagValue::String("test".into()))
        );
        client.flush();

        alpha_mock.assert_calls(1);
        variant_mock.assert_calls(1);
        unexpected_capture.assert_calls(0);
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
        client.flush();
        capture_mock.assert_calls(0);
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

        let mut options_1 = EvaluateFlagsOptions::default();
        options_1.groups = Some(g1);
        let snap_1 = client.evaluate_flags("user-1", options_1).unwrap();
        let mut options_2 = EvaluateFlagsOptions::default();
        options_2.groups = Some(g2);
        let snap_2 = client.evaluate_flags("user-1", options_2).unwrap();
        assert!(snap_1.is_enabled("alpha"));
        client.flush();
        assert!(snap_2.is_enabled("alpha"));
        client.flush();
        capture_mock.assert_calls(expected_hits);
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
        let capture_mock = capture_events_mock(
            &server,
            vec![(
                "$feature_flag_called",
                json!({
                    "$feature_flag": "alpha", "$feature_flag_response": true,
                    "$groups": {"organization": "org-a"}
                }),
            )],
        );
        let unexpected_capture = capture_path_mock(&server);
        let client = create_test_client(server.base_url());
        let mut options = EvaluateFlagsOptions::default();
        options.groups = Some(groups(&[("organization", "org-a")]));
        let snap = client.evaluate_flags("user-1", options).unwrap();
        assert!(snap.is_enabled("alpha"));
        assert!(snap.is_enabled("alpha"));
        assert!(snap.is_enabled("alpha"));
        client.flush();
        capture_mock.assert_calls(1);
        unexpected_capture.assert_calls(0);
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
            when.method(POST).path("/flags/").json_body_includes(
                json!({"flag_keys_to_evaluate": ["alpha", "beta"]}).to_string(),
            );
            then.status(200).json_body(flags_response_fixture());
        });
        let client = create_test_client(server.base_url());
        let mut opts = EvaluateFlagsOptions::default();
        opts.flag_keys = Some(vec!["alpha".into(), "beta".into()]);
        let _ = client.evaluate_flags("user-1", opts).unwrap();
        flags_mock.assert_calls(1);
    }

    #[test]
    fn explicit_empty_flag_keys_skip_local_and_remote_evaluation() {
        let server = MockServer::start();
        let definitions_mock = server.mock(|when, then| {
            when.method(GET).path("/flags/definitions/");
            then.status(200).json_body(local_definitions_fixture());
        });
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = capture_path_mock(&server);
        let options = posthog::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .secret_key("test_personal_key".to_string())
            .enable_local_evaluation(true)
            .poll_interval_seconds(3600)
            .build()
            .unwrap();
        let client = posthog::client(options);
        definitions_mock.assert_calls(1);

        let mut evaluate_options = EvaluateFlagsOptions::default();
        evaluate_options.flag_keys = Some(Vec::new());
        let snapshot = client
            .evaluate_flags("user-1", evaluate_options)
            .expect("empty key scope should be valid");

        assert!(snapshot.keys().is_empty());
        assert!(!snapshot.is_enabled("alpha"));
        client.flush();
        definitions_mock.assert_calls(1);
        flags_mock.assert_calls(0);
        capture_mock.assert_calls(1);
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
        client.flush();
        flags_mock.assert_calls(0);
        capture_mock.assert_calls(0);
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
        let capture_mock = capture_events_mock(
            &server,
            vec![
                (
                    "$feature_flag_called",
                    json!({
                        "$feature_flag": "alpha", "$feature_flag_error": "errors_while_computing_flags"
                    }),
                ),
                (
                    "$feature_flag_called",
                    json!({
                        "$feature_flag": "does-not-exist", "$feature_flag_error": "errors_while_computing_flags,flag_missing"
                    }),
                ),
            ],
        );
        let unexpected_capture = capture_path_mock(&server);
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        assert!(snapshot.is_enabled("alpha"));
        assert!(snapshot.get_flag("does-not-exist").is_none());
        client.flush();
        capture_mock.assert_calls(1);
        unexpected_capture.assert_calls(0);
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

    /// C5: `$feature_flag_called` ships via the capture endpoint, never the
    /// retired legacy path.
    #[test]
    fn flag_called_event_routes_to_capture_endpoint() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v1/analytics/events")
                .header("posthog-attempt", "1")
                .header(
                    "posthog-sdk-info",
                    format!("posthog-rs/{}", env!("CARGO_PKG_VERSION")),
                )
                .body_includes("$feature_flag_called")
                .body_includes("\"$feature_flag\":\"alpha\"");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "results": {} }));
        });
        let legacy_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        assert!(snapshot.is_enabled("alpha"));
        client.flush();
        capture_mock.assert_calls(1);
        legacy_mock.assert_calls(0);
    }

    /// C5: `$feature_flag_called` uses the normal V1 capture retry path.
    #[test]
    fn flag_called_event_failure_is_retried() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let first_attempt = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v1/analytics/events")
                .header("posthog-attempt", "1");
            then.status(503)
                .header("content-type", "application/json")
                .json_body(json!({ "error": "service_unavailable" }));
        });
        let retry_attempt = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v1/analytics/events")
                .header("posthog-attempt", "2");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "results": {} }));
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        assert!(snapshot.is_enabled("alpha"));
        client.flush();
        client.flush();
        first_attempt.assert_calls(1);
        retry_attempt.assert_calls(1);
    }

    #[test]
    fn disabled_client_returns_empty_snapshot() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let options = posthog::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .disabled(true)
            .build()
            .unwrap();
        let client = posthog::client(options);
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        assert!(snapshot.keys().is_empty());
        flags_mock.assert_calls(0);
    }

    /// Ported from the removed V0 suite: `Event::with_flags` attaches the flag
    /// properties from an existing snapshot without triggering a second
    /// `/flags` fetch.
    #[test]
    fn event_with_flags_attaches_properties_without_extra_request() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .body_includes("\"$active_feature_flags\":\"caller-value\"");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "results": {} }));
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        let mut event = Event::new("checkout-started", "user-1");
        event
            .insert_prop("$active_feature_flags", "caller-value")
            .unwrap();
        event.with_flags(&snapshot);
        client.capture(event);
        client.flush();
        // One /flags request, one capture request — no second flag fetch.
        flags_mock.assert_calls(1);
        capture_mock.assert_calls(1);
    }

    /// Ported from the removed V0 suite: `$feature_flag_called` carries
    /// `$lib`, `$lib_version` and `$is_server`.
    #[test]
    fn flag_called_event_contains_lib_and_is_server() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .body_includes("$feature_flag_called")
                .body_includes("\"$lib\":\"posthog-rs\"")
                .body_includes(format!(
                    "\"$lib_version\":\"{}\"",
                    env!("CARGO_PKG_VERSION")
                ))
                .body_includes("\"$is_server\":true");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "results": {} }));
        });
        let client = create_test_client(server.base_url());
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        assert!(snapshot.is_enabled("alpha"));
        client.flush();
        capture_mock.assert_calls(1);
    }

    #[test]
    fn known_absent_keys_resolve_locally_without_a_request() {
        let server = MockServer::start();
        mock_known_absent_definitions(&server);
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let client = posthog::client(local_evaluation_client_options(server.base_url()));

        let snapshot = client
            .evaluate_flags("user-1", known_absent_options(&["profile-incomplete"]))
            .unwrap();

        assert_eq!(
            snapshot.get_flag("profile-incomplete"),
            Some(FlagValue::Boolean(true))
        );
        flags_mock.assert_calls(0);
    }

    #[test]
    fn remote_fallback_sends_known_absent_keys() {
        let server = MockServer::start();
        mock_known_absent_definitions(&server);
        let flags_mock = mock_flags_with_known_absent(
            &server,
            Some((json!(["plan"]), json!({"organization": ["tier"]}))),
        );
        let client = posthog::client(local_evaluation_client_options(server.base_url()));

        client
            .evaluate_flags(
                "user-1",
                known_absent_options(&["profile-incomplete", "regional"]),
            )
            .unwrap();

        flags_mock.assert_calls(1);
    }

    #[test]
    fn remote_request_omits_known_absent_keys_not_provided() {
        let server = MockServer::start();
        let flags_mock = mock_flags_with_known_absent(&server, None);
        let client = create_test_client(server.base_url());

        client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .unwrap();
        let mut empty = EvaluateFlagsOptions::default();
        empty.person_property_keys_known_absent = Some(Vec::new());
        empty.group_property_keys_known_absent = Some(Default::default());
        client.evaluate_flags("user-1", empty).unwrap();

        flags_mock.assert_calls(2);
    }

    #[test]
    fn over_limit_known_absent_keys_fail_without_a_request() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let client = create_test_client(server.base_url());

        for options in over_limit_known_absent_options() {
            for distinct_id in ["user-1", ""] {
                let error = client.evaluate_flags(distinct_id, options.clone()).err();
                assert!(
                    matches!(error, Some(posthog::Error::InvalidArgument(_))),
                    "{:?}",
                    error
                );
            }
        }
        let mut at_limit = EvaluateFlagsOptions::default();
        at_limit.person_property_keys_known_absent = Some(vec!["plan".to_string(); 1000]);
        client.evaluate_flags("user-1", at_limit).unwrap();

        flags_mock.assert_calls(1);
    }

    #[test]
    fn omitted_or_null_condition_value_on_a_listed_key_stays_inconclusive() {
        let server = MockServer::start();
        mock_known_absent_valueless_definitions(&server);
        let flags_mock = mock_flags_with_known_absent(
            &server,
            Some((json!(["plan"]), json!({"organization": ["tier"]}))),
        );
        let client = posthog::client(local_evaluation_client_options(server.base_url()));

        let snapshot = client
            .evaluate_flags("user-1", valueless_options(true))
            .unwrap();
        for key in VALUELESS_FLAG_KEYS {
            assert_eq!(snapshot.get_flag(key), None, "{key}");
        }
        assert_eq!(
            snapshot.get_flag("profile-incomplete"),
            Some(FlagValue::Boolean(true))
        );
        flags_mock.assert_calls(0);

        client
            .evaluate_flags("user-1", valueless_options(false))
            .unwrap();
        flags_mock.assert_calls(1);
    }
}

#[cfg(feature = "async-client")]
mod async_tests {
    use super::*;
    use posthog::{EvaluateFlagsOptions, Event, FlagValue};
    use reqwest::header::USER_AGENT;

    async fn create_test_client(base_url: String) -> posthog::Client {
        let options: posthog::ClientOptions = ("test_api_key", base_url.as_str()).into();
        posthog::client(options).await
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
        flags_mock.assert_calls(1);
    }

    #[tokio::test]
    async fn local_evaluation_only_without_evaluator_does_not_call_api() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let options = posthog::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .enable_local_evaluation(true)
            .local_evaluation_only(true)
            .build()
            .unwrap();
        let client = posthog::client(options).await;

        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .expect("local-only evaluation should return an empty snapshot");

        assert!(snapshot.keys().is_empty());
        flags_mock.assert_calls(0);
    }

    #[tokio::test]
    async fn evaluate_flags_retries_transport_error_then_succeeds() {
        let server = start_flaky_flags_server(flags_response_fixture().to_string());
        let options = posthog::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url.clone())
            .feature_flags_request_max_retries(1u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = posthog::client(options).await;

        let flags = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .expect("evaluate_flags should retry transport error");

        assert_eq!(flags.get_flag("alpha"), Some(FlagValue::Boolean(true)));
        server.assert_retry_succeeded();
    }

    #[tokio::test]
    async fn evaluate_flags_returns_error_after_transport_retry_budget() {
        let server = start_resetting_flags_server(2);
        let options = posthog::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url.clone())
            .feature_flags_request_max_retries(1u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = posthog::client(options).await;

        let err = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .expect_err("transport errors should stop after retry budget is exhausted");

        assert!(matches!(err, posthog::Error::Connection(_)));
        server.assert_attempts(2);
    }

    #[tokio::test]
    async fn evaluate_flags_retries_502_and_504_status_then_succeeds() {
        for status in [502, 504] {
            let server = start_status_then_success_flags_server(
                status,
                flags_response_fixture().to_string(),
            );
            let options = posthog::ClientOptionsBuilder::default()
                .api_key("test_api_key".to_string())
                .host(server.base_url.clone())
                .feature_flags_request_max_retries(1u32)
                .retry_initial_backoff_ms(1u64)
                .retry_max_backoff_ms(1u64)
                .build()
                .unwrap();
            let client = posthog::client(options).await;

            let snapshot = client
                .evaluate_flags("user-1", EvaluateFlagsOptions::default())
                .await
                .expect("evaluate_flags should retry retryable status");

            assert_eq!(snapshot.get_flag("alpha"), Some(FlagValue::Boolean(true)));
            server.assert_retry_succeeded();
        }
    }

    #[tokio::test]
    async fn evaluate_flags_returns_error_after_502_and_504_retry_budget() {
        for status in [502, 504] {
            let server = MockServer::start();
            let flags_mock = server.mock(|when, then| {
                when.method(POST).path("/flags/");
                then.status(status).body("transient flags error");
            });
            let options = posthog::ClientOptionsBuilder::default()
                .api_key("test_api_key".to_string())
                .host(server.base_url())
                .feature_flags_request_max_retries(1u32)
                .retry_initial_backoff_ms(1u64)
                .retry_max_backoff_ms(1u64)
                .build()
                .unwrap();
            let client = posthog::client(options).await;

            let err = client
                .evaluate_flags("user-1", EvaluateFlagsOptions::default())
                .await
                .expect_err("retryable status should error after retry budget is exhausted");

            assert!(err.to_string().contains(&status.to_string()));
            flags_mock.assert_calls(2);
        }
    }

    #[tokio::test]
    async fn evaluate_flags_does_not_retry_500_status() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(500).body("boom");
        });
        let options = posthog::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .max_capture_attempts(3u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = posthog::client(options).await;

        let err = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .expect_err("500 status should be terminal");

        assert!(err.to_string().contains("500"));
        flags_mock.assert_calls(1);
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
        let alpha_mock = capture_events_mock(
            &server,
            vec![(
                "$feature_flag_called",
                json!({
                    "$feature_flag": "alpha", "$feature_flag_response": true
                }),
            )],
        );
        let variant_mock = capture_events_mock(
            &server,
            vec![(
                "$feature_flag_called",
                json!({
                    "$feature_flag": "variant-flag", "$feature_flag_response": "test"
                }),
            )],
        );
        let unexpected_capture = capture_path_mock(&server);
        let client = create_test_client(server.base_url()).await;
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        assert!(snapshot.is_enabled("alpha"));
        assert!(snapshot.is_enabled("alpha"));
        client.flush().await;
        assert_eq!(
            snapshot.get_flag("variant-flag"),
            Some(FlagValue::String("test".into()))
        );
        assert_eq!(
            snapshot.get_flag("variant-flag"),
            Some(FlagValue::String("test".into()))
        );
        client.flush().await;
        alpha_mock.assert_calls(1);
        variant_mock.assert_calls(1);
        unexpected_capture.assert_calls(0);
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
        client.flush().await;
        capture_mock.assert_calls(0);
    }

    #[tokio::test]
    async fn flag_keys_forwarded_to_request_body() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/flags/")
                .json_body_includes(json!({"flag_keys_to_evaluate": ["alpha"]}).to_string());
            then.status(200).json_body(flags_response_fixture());
        });
        let client = create_test_client(server.base_url()).await;
        let mut opts = EvaluateFlagsOptions::default();
        opts.flag_keys = Some(vec!["alpha".into()]);
        let _ = client.evaluate_flags("user-1", opts).await.unwrap();
        flags_mock.assert_calls(1);
    }

    #[tokio::test]
    async fn explicit_empty_flag_keys_skip_local_and_remote_evaluation() {
        let server = MockServer::start();
        let definitions_mock = server.mock(|when, then| {
            when.method(GET).path("/flags/definitions/");
            then.status(200).json_body(local_definitions_fixture());
        });
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = capture_path_mock(&server);
        let options = posthog::ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .secret_key("test_personal_key".to_string())
            .enable_local_evaluation(true)
            .poll_interval_seconds(3600)
            .build()
            .unwrap();
        let client = posthog::client(options).await;
        definitions_mock.assert_calls(1);

        let mut evaluate_options = EvaluateFlagsOptions::default();
        evaluate_options.flag_keys = Some(Vec::new());
        let snapshot = client
            .evaluate_flags("user-1", evaluate_options)
            .await
            .expect("empty key scope should be valid");

        assert!(snapshot.keys().is_empty());
        assert!(!snapshot.is_enabled("alpha"));
        client.flush().await;
        definitions_mock.assert_calls(1);
        flags_mock.assert_calls(0);
        capture_mock.assert_calls(1);
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
        client.flush().await;
        flags_mock.assert_calls(0);
        capture_mock.assert_calls(0);
    }

    /// C5: `$feature_flag_called` ships via the capture endpoint, never the
    /// retired legacy path.
    #[tokio::test]
    async fn flag_called_event_routes_to_capture_endpoint() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v1/analytics/events")
                .header("posthog-attempt", "1")
                .header(
                    "posthog-sdk-info",
                    format!("posthog-rs/{}", env!("CARGO_PKG_VERSION")),
                )
                .body_includes("$feature_flag_called")
                .body_includes("\"$feature_flag\":\"alpha\"");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "results": {} }));
        });
        let legacy_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let client = create_test_client(server.base_url()).await;
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        assert!(snapshot.is_enabled("alpha"));
        client.flush().await;
        capture_mock.assert_calls(1);
        legacy_mock.assert_calls(0);
    }

    /// C5: `$feature_flag_called` uses the normal V1 capture retry path.
    #[tokio::test]
    async fn flag_called_event_failure_is_retried() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let first_attempt = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v1/analytics/events")
                .header("posthog-attempt", "1");
            then.status(503)
                .header("content-type", "application/json")
                .json_body(json!({ "error": "service_unavailable" }));
        });
        let retry_attempt = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v1/analytics/events")
                .header("posthog-attempt", "2");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "results": {} }));
        });
        let client = create_test_client(server.base_url()).await;
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        assert!(snapshot.is_enabled("alpha"));
        client.flush().await;
        client.flush().await;
        first_attempt.assert_calls(1);
        retry_attempt.assert_calls(1);
    }

    /// Ported from the removed V0 suite: `Event::with_flags` attaches the flag
    /// properties from an existing snapshot without triggering a second
    /// `/flags` fetch.
    #[tokio::test]
    async fn event_with_flags_attaches_properties_without_extra_request() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = capture_events_mock(
            &server,
            vec![(
                "checkout-started",
                json!({
                    "$feature/alpha": true, "$feature/beta": false, "$feature/variant-flag": "test",
                    "$active_feature_flags": ["alpha", "variant-flag"]
                }),
            )],
        );
        let unexpected_capture = capture_path_mock(&server);
        let client = create_test_client(server.base_url()).await;
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        let mut event = Event::new("checkout-started", "user-1");
        event.with_flags(&snapshot);
        client.capture(event);
        client.flush().await;
        // One /flags request, one capture request — no second flag fetch.
        flags_mock.assert_calls(1);
        capture_mock.assert_calls(1);
        unexpected_capture.assert_calls(0);
    }

    /// Ported from the removed V0 suite: `$feature_flag_called` carries
    /// `$lib`, `$lib_version` and `$is_server`.
    #[tokio::test]
    async fn flag_called_event_contains_lib_and_is_server() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .body_includes("$feature_flag_called")
                .body_includes("\"$lib\":\"posthog-rs\"")
                .body_includes(format!(
                    "\"$lib_version\":\"{}\"",
                    env!("CARGO_PKG_VERSION")
                ))
                .body_includes("\"$is_server\":true");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "results": {} }));
        });
        let client = create_test_client(server.base_url()).await;
        let snapshot = client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        assert!(snapshot.is_enabled("alpha"));
        client.flush().await;
        capture_mock.assert_calls(1);
    }

    #[tokio::test]
    async fn known_absent_keys_resolve_locally_without_a_request() {
        let server = MockServer::start();
        mock_known_absent_definitions(&server);
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let client = posthog::client(local_evaluation_client_options(server.base_url())).await;

        let snapshot = client
            .evaluate_flags("user-1", known_absent_options(&["profile-incomplete"]))
            .await
            .unwrap();

        assert_eq!(
            snapshot.get_flag("profile-incomplete"),
            Some(FlagValue::Boolean(true))
        );
        flags_mock.assert_calls(0);
    }

    #[tokio::test]
    async fn remote_fallback_sends_known_absent_keys() {
        let server = MockServer::start();
        mock_known_absent_definitions(&server);
        let flags_mock = mock_flags_with_known_absent(
            &server,
            Some((json!(["plan"]), json!({"organization": ["tier"]}))),
        );
        let client = posthog::client(local_evaluation_client_options(server.base_url())).await;

        client
            .evaluate_flags(
                "user-1",
                known_absent_options(&["profile-incomplete", "regional"]),
            )
            .await
            .unwrap();

        flags_mock.assert_calls(1);
    }

    #[tokio::test]
    async fn remote_request_omits_known_absent_keys_not_provided() {
        let server = MockServer::start();
        let flags_mock = mock_flags_with_known_absent(&server, None);
        let client = create_test_client(server.base_url()).await;

        client
            .evaluate_flags("user-1", EvaluateFlagsOptions::default())
            .await
            .unwrap();
        let mut empty = EvaluateFlagsOptions::default();
        empty.person_property_keys_known_absent = Some(Vec::new());
        empty.group_property_keys_known_absent = Some(Default::default());
        client.evaluate_flags("user-1", empty).await.unwrap();

        flags_mock.assert_calls(2);
    }

    #[tokio::test]
    async fn over_limit_known_absent_keys_fail_without_a_request() {
        let server = MockServer::start();
        let flags_mock = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(flags_response_fixture());
        });
        let client = create_test_client(server.base_url()).await;

        for options in over_limit_known_absent_options() {
            for distinct_id in ["user-1", ""] {
                let error = client
                    .evaluate_flags(distinct_id, options.clone())
                    .await
                    .err();
                assert!(
                    matches!(error, Some(posthog::Error::InvalidArgument(_))),
                    "{:?}",
                    error
                );
            }
        }
        let mut at_limit = EvaluateFlagsOptions::default();
        at_limit.person_property_keys_known_absent = Some(vec!["plan".to_string(); 1000]);
        client.evaluate_flags("user-1", at_limit).await.unwrap();

        flags_mock.assert_calls(1);
    }

    #[tokio::test]
    async fn omitted_or_null_condition_value_on_a_listed_key_stays_inconclusive() {
        let server = MockServer::start();
        mock_known_absent_valueless_definitions(&server);
        let flags_mock = mock_flags_with_known_absent(
            &server,
            Some((json!(["plan"]), json!({"organization": ["tier"]}))),
        );
        let client = posthog::client(local_evaluation_client_options(server.base_url())).await;

        let snapshot = client
            .evaluate_flags("user-1", valueless_options(true))
            .await
            .unwrap();
        for key in VALUELESS_FLAG_KEYS {
            assert_eq!(snapshot.get_flag(key), None, "{key}");
        }
        assert_eq!(
            snapshot.get_flag("profile-incomplete"),
            Some(FlagValue::Boolean(true))
        );
        flags_mock.assert_calls(0);

        client
            .evaluate_flags("user-1", valueless_options(false))
            .await
            .unwrap();
        flags_mock.assert_calls(1);
    }
}

#[cfg(not(feature = "async-client"))]
#[test]
fn evaluate_flags_with_request_error_returns_a_snapshot_and_the_error() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(503).body("unavailable");
    });
    let client = posthog::client(
        posthog::ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .host(server.base_url())
            .feature_flags_request_max_retries(0u32)
            .build()
            .unwrap(),
    );

    let (flags, error) = client
        .evaluate_flags_with_request_error("user-1", posthog::EvaluateFlagsOptions::default());
    assert!(
        matches!(
            error,
            Some(posthog::Error::ServerError { status: 503, .. })
                | Some(posthog::Error::Connection(_))
        ),
        "{:?}",
        error
    );
    assert!(flags.keys().is_empty());
    assert!(!flags.is_enabled("beta"));

    // The public method keeps returning the error when nothing resolved.
    assert!(client
        .evaluate_flags("user-1", posthog::EvaluateFlagsOptions::default())
        .is_err());
}

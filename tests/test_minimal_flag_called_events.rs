//! End-to-end coverage for minimized `$feature_flag_called` events over the
//! remote `/flags?v=2` path: the server-controlled `minimalFlagCalledEvents`
//! gate plus each flag's `has_experiment` decide whether the captured event
//! keeps its full property set or is trimmed to the strict allowlist.
//!
//! Uses a small recording HTTP server (rather than httpmock) so the exact
//! `$feature_flag_called` request body can be inspected.

#![cfg(feature = "async-client")]

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};

use posthog_rs::EvaluateFlagsOptions;

#[cfg(feature = "capture-v1")]
const CAPTURE_PATH: &str = "/i/v1/analytics/events";
#[cfg(not(feature = "capture-v1"))]
const CAPTURE_PATH: &str = "/batch/";

/// The full minimal-event allowlist, mirrored from the crate-internal constant
/// so this black-box test can assert nothing outside it survives.
const ALLOWLIST: &[&str] = &[
    "$feature_flag",
    "$feature_flag_response",
    "$feature_flag_has_experiment",
    "$feature_flag_id",
    "$feature_flag_version",
    "$feature_flag_reason",
    "$feature_flag_request_id",
    "$feature_flag_evaluated_at",
    "$feature_flag_error",
    "locally_evaluated",
    "$groups",
    "$process_person_profile",
    "$geoip_disable",
    "$session_id",
    "$window_id",
    "$device_id",
    "$lib",
    "$lib_version",
    "$is_server",
    "$os",
    "$os_version",
];

fn flags_fixture(gate: bool) -> Value {
    let mut body = json!({
        "flags": {
            "plain": {
                "key": "plain",
                "enabled": true,
                "variant": null,
                "reason": { "code": "condition_match", "description": "matched", "condition_index": 0 },
                "metadata": { "id": 1, "version": 2, "description": null, "payload": null, "has_experiment": false }
            },
            "experiment": {
                "key": "experiment",
                "enabled": true,
                "variant": "test",
                "reason": { "code": "condition_match", "description": "matched", "condition_index": 0 },
                "metadata": { "id": 2, "version": 3, "description": null, "payload": null, "has_experiment": true }
            }
        },
        "requestId": "req-xyz"
    });
    if gate {
        body["minimalFlagCalledEvents"] = json!(true);
    }
    body
}

/// A minimal recording HTTP server that answers `/flags/` with the fixture and
/// records the bodies POSTed to the capture endpoint.
struct RecordingServer {
    base_url: String,
    captured: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for RecordingServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<(String, Vec<u8>)> {
    stream.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    // Read until headers are complete.
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return None,
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    Some((headers, body))
}

fn start_recording_server(gate: bool) -> RecordingServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind recording server");
    listener.set_nonblocking(true).expect("set nonblocking");
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let captured = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));

    let thread_captured = Arc::clone(&captured);
    let thread_stop = Arc::clone(&stop);
    let flags_body = flags_fixture(gate).to_string();

    let handle = thread::spawn(move || {
        while !thread_stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let Some((headers, body)) = read_request(&mut stream) else {
                        continue;
                    };
                    let request_line = headers.lines().next().unwrap_or("");
                    let response_body = if request_line.contains("/flags/") {
                        flags_body.clone()
                    } else {
                        if request_line.contains(CAPTURE_PATH) {
                            thread_captured.lock().unwrap().push(body);
                        }
                        "{\"results\":{}}".to_string()
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        response_body.len(),
                        response_body
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });

    RecordingServer {
        base_url,
        captured,
        stop,
        handle: Some(handle),
    }
}

/// Drive one flag evaluation through the real client and return the properties
/// of the captured `$feature_flag_called` event.
async fn captured_flag_called_properties(
    gate: bool,
    flag_key: &str,
) -> serde_json::Map<String, Value> {
    let server = start_recording_server(gate);

    let options: posthog_rs::ClientOptions = ("phc_test", server.base_url.as_str()).into();
    let client = posthog_rs::client(options).await;
    let snapshot = client
        .evaluate_flags("user-1", EvaluateFlagsOptions::default())
        .await
        .expect("evaluate_flags");
    let _ = snapshot.is_enabled(flag_key);
    client.flush().await;

    // Poll briefly for the capture request the background worker sends.
    let started = Instant::now();
    loop {
        if let Some(props) = extract_flag_called(&server.captured.lock().unwrap()) {
            return props;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "no $feature_flag_called event was captured"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn extract_flag_called(bodies: &[Vec<u8>]) -> Option<serde_json::Map<String, Value>> {
    for body in bodies {
        let parsed: Value = serde_json::from_slice(body).ok()?;
        let batch = parsed.get("batch").and_then(Value::as_array)?;
        for event in batch {
            if event.get("event").and_then(Value::as_str) == Some("$feature_flag_called") {
                return event.get("properties").and_then(Value::as_object).cloned();
            }
        }
    }
    None
}

#[tokio::test]
async fn remote_gate_on_no_experiment_sends_minimal_event() {
    let props = captured_flag_called_properties(true, "plain").await;

    for key in props.keys() {
        assert!(
            ALLOWLIST.contains(&key.as_str()),
            "unexpected key leaked: {}",
            key
        );
    }
    assert_eq!(props.get("$feature_flag"), Some(&json!("plain")));
    assert_eq!(props.get("$feature_flag_response"), Some(&json!(true)));
    assert_eq!(
        props.get("$feature_flag_has_experiment"),
        Some(&json!(false))
    );
    assert_eq!(props.get("locally_evaluated"), Some(&json!(false)));
    // The bulky per-flag mirror is stripped.
    assert!(!props.contains_key("$feature/plain"));
}

#[tokio::test]
async fn remote_experiment_linked_keeps_full_event() {
    let props = captured_flag_called_properties(true, "experiment").await;
    // Experiment-linked flags always keep the full shape for exposure analysis.
    assert_eq!(
        props.get("$feature_flag_has_experiment"),
        Some(&json!(true))
    );
    assert_eq!(props.get("$feature/experiment"), Some(&json!("test")));
}

#[tokio::test]
async fn remote_gate_off_keeps_full_event() {
    let props = captured_flag_called_properties(false, "plain").await;
    // Gate absent -> full event even though the flag has no experiment.
    assert_eq!(
        props.get("$feature_flag_has_experiment"),
        Some(&json!(false))
    );
    assert_eq!(props.get("$feature/plain"), Some(&json!(true)));
}

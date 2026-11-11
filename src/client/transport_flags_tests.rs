use super::*;
use crate::client::transport::storage_tests::wire;
use crate::client::transport::{Admission, TransportHandle};
use crate::client::ClientOptionsBuilder;
use httpmock::{Method::POST, MockServer};
use serde_json::json;
use std::sync::mpsc;
use std::thread;

const WAIT: Duration = Duration::from_secs(5);

fn options(host: String) -> ClientOptions {
    ClientOptionsBuilder::default()
        .api_key("phc_test".to_string())
        .host(host)
        .flush_at(100usize)
        .flush_interval_ms(60_000u64)
        .feature_flags_request_max_retries(0u32)
        .feature_flags_request_timeout_seconds(1u64)
        .shutdown_timeout_ms(1000u64)
        .build()
        .unwrap()
}

fn response(payload: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "flags":{"checkout":{"key":"checkout","enabled":true,"variant":"blue",
            "metadata":{"id":12,"version":3,"payload":payload,"has_experiment":false}}},
        "requestId":"request-a","evaluatedAt":123,"minimalFlagCalledEvents":true,
        "errorsWhileComputingFlags":true
    }))
    .unwrap()
}

#[test]
fn v2_payloads_decode_once_with_reporting_metadata() {
    for (raw, expected) in [
        ("null", Value::Null),
        ("false", json!(false)),
        ("0", json!(0)),
        ("\"123\"", json!("123")),
        ("\"true\"", json!("true")),
        ("\"\"", json!("")),
        ("[1,null]", json!([1, null])),
        ("{\"a\":null}", json!({"a":null})),
    ] {
        let loaded = decode(&response(json!(raw))).unwrap();
        let record = &loaded.records["checkout"];
        assert_eq!(record.payload, Some(expected));
        assert_eq!(record.variant.as_deref(), Some("blue"));
        assert_eq!(
            (record.id, record.version, record.has_experiment),
            (Some(12), Some(3), Some(false))
        );
        assert!(record.minimal_flag_called_events);
        assert!(loaded.partial);
        assert!(!loaded.invalid_payloads);
        assert_eq!(loaded.request_id.as_deref(), Some("request-a"));
        assert_eq!(loaded.evaluated_at, Some(123));
    }
}

#[test]
fn malformed_wire_payload_is_absent_in_both_response_formats() {
    for raw in ["", "invalid sensitive data", "{broken"] {
        for bytes in [
            response(json!(raw)),
            serde_json::to_vec(&json!({"featureFlags":{"checkout":true},
                "featureFlagPayloads":{"checkout":raw}}))
            .unwrap(),
        ] {
            let loaded = decode(&bytes).unwrap();
            assert!(loaded.invalid_payloads);
            assert_eq!(loaded.records["checkout"].payload, None);
            assert!(loaded.records["checkout"].enabled);
        }
    }
}

#[test]
fn legacy_wire_payloads_decode_once_without_synthetic_metadata() {
    for (value, expected) in [
        (json!("null"), Value::Null),
        (json!("false"), json!(false)),
        (json!("0"), json!(0)),
        (json!("\"123\""), json!("123")),
        (json!("\"true\""), json!("true")),
        (json!("\"\""), json!("")),
        (json!("[1,null]"), json!([1, null])),
        (json!("{\"a\":null}"), json!({"a":null})),
        (Value::Null, Value::Null),
        (json!(false), json!(false)),
        (json!(0), json!(0)),
        (json!([1, null]), json!([1, null])),
        (json!({"a":null}), json!({"a":null})),
    ] {
        let loaded = decode(
            &serde_json::to_vec(&json!({
                "featureFlags":{"checkout":false},
                "featureFlagPayloads":{"checkout":value}
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(loaded.records["checkout"].payload, Some(expected));
        assert_eq!(loaded.records["checkout"].id, None);
        assert_eq!(loaded.records["checkout"].version, None);
        assert!(!loaded.records["checkout"].enabled);
        assert!(!loaded.invalid_payloads);
    }
}

#[test]
fn failed_v2_evaluations_are_not_successful_false_records() {
    for partial in [Some(true), Some(false), None] {
        let mut response = json!({"flags":{
            "failed":{"key":"failed","enabled":false,"failed":true},
            "disabled":{"key":"disabled","enabled":false,"failed":false},
            "enabled":{"key":"enabled","enabled":true}
        }});
        if let Some(partial) = partial {
            response["errorsWhileComputingFlags"] = json!(partial);
        }
        let loaded = decode(&serde_json::to_vec(&response).unwrap()).unwrap();
        assert!(
            !loaded.records.contains_key("failed"),
            "failed evaluation must not replace cached value"
        );
        assert!(!loaded.records["disabled"].enabled);
        assert!(loaded.records["enabled"].enabled);
        assert!(
            loaded.partial,
            "failed records require preservation of unrecomputed values"
        );
    }
}

#[test]
fn legacy_envelope_preserves_partial_request_and_reporting_metadata() {
    let loaded = decode(
        &serde_json::to_vec(&json!({
            "featureFlags":{"checkout":false},
            "featureFlagPayloads":{"checkout":"123"},
            "errorsWhileComputingFlags":true,"requestId":"legacy-request",
            "evaluatedAt":456,"minimalFlagCalledEvents":true
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(
        loaded.partial,
        "legacy envelope must retain partial-result semantics"
    );
    assert_eq!(loaded.request_id.as_deref(), Some("legacy-request"));
    assert_eq!(loaded.evaluated_at, Some(456));
    assert!(loaded.records["checkout"].minimal_flag_called_events);
    assert_eq!(loaded.records["checkout"].payload, Some(json!(123)));
    assert!(!loaded.records["checkout"].enabled);
    assert_eq!(loaded.records["checkout"].id, None);
    let historical =
        decode(br#"{"featureFlags":{},"errorsWhileComputingFlags":false,"errors":["failed"]}"#)
            .unwrap();
    assert!(historical.partial);
    assert!(historical.request_id.is_none());
}

#[test]
fn decoder_validates_envelope_and_failed_markers_before_filtering() {
    for base in [json!({"flags":{}}), json!({"featureFlags":{}})] {
        for (key, bad) in [
            ("errorsWhileComputingFlags", json!("true")),
            ("minimalFlagCalledEvents", Value::Null),
            ("requestId", json!(7)),
            ("evaluatedAt", json!(1.5)),
        ] {
            let mut response = base.clone();
            response[key] = bad;
            assert!(matches!(
                decode(&serde_json::to_vec(&response).unwrap()),
                Err(LoadError::InvalidResponse)
            ));
        }
    }
    for failed in [Value::Null, json!("true"), json!(1)] {
        let response = json!({"flags":{"flag":{"key":"flag","enabled":false,"failed":failed}}});
        assert!(matches!(
            decode(&serde_json::to_vec(&response).unwrap()),
            Err(LoadError::InvalidResponse)
        ));
    }
    let flags: HashMap<_, _> = (0..MAX_FLAGS + 1)
        .map(|i| (format!("failed{i}"), json!({"failed":true})))
        .collect();
    assert!(matches!(
        decode(&serde_json::to_vec(&json!({"flags":flags})).unwrap()),
        Err(LoadError::TooManyFlags)
    ));
    let failed_only = decode(br#"{"flags":{"failed":{"failed":true}}}"#).unwrap();
    assert!(failed_only.partial);
    assert!(failed_only.records.is_empty());
}

#[test]
fn decoder_bounds_and_failure_classification() {
    for bytes in [b"{}".as_slice(), b"not json", b"{\"flags\":false}"] {
        assert!(matches!(decode(bytes), Err(LoadError::InvalidResponse)));
    }
    assert!(matches!(
        decode(&vec![b' '; MAX_RESPONSE_BYTES + 1]),
        Err(LoadError::ResponseTooLarge)
    ));
    assert!(matches!(
        decode(br#"{"flags":{},"quotaLimited":true}"#),
        Err(LoadError::QuotaLimited)
    ));
    assert!(matches!(
        decode(br#"{"quotaLimited":["feature_flags"]}"#),
        Err(LoadError::QuotaLimited)
    ));
    assert!(matches!(
        decode(br#"{"flags":{},"quotaLimited":[1]}"#),
        Err(LoadError::InvalidResponse)
    ));
    assert!(decode(br#"{"flags":{},"quotaLimited":["recordings"]}"#)
        .unwrap()
        .records
        .is_empty());
    assert!(decode(br#"{"flags":{},"quotaLimited":[]}"#)
        .unwrap()
        .records
        .is_empty());
    let many: HashMap<_, _> = (0..MAX_FLAGS + 1)
        .map(|n| (format!("key{n}"), false))
        .collect();
    assert!(matches!(
        decode(&serde_json::to_vec(&json!({"featureFlags":many})).unwrap()),
        Err(LoadError::TooManyFlags)
    ));
    assert!(matches!(
        Task::new(&json!({"large":"x".repeat(MAX_REQUEST_BYTES)}), |_| {}),
        Err(LoadError::RequestTooLarge)
    ));
    assert!(decode(br#"{"flags":{}}"#).unwrap().records.is_empty());
}

#[test]
fn pending_slot_uses_one_token_and_retains_only_latest_task() {
    let slot = Arc::new(Slot::default());
    let closed = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel();
    for id in 0..1000 {
        assert!(slot.enqueue(
            Task::new(&json!({"distinct_id":id.to_string()}), |_| {}).unwrap(),
            &tx,
            &closed
        ));
    }
    assert_eq!(rx.try_iter().count(), 1);
    let pending = slot.0.lock().unwrap();
    assert_eq!(
        pending.task.as_ref().unwrap().distinct_id.as_deref(),
        Some("999")
    );
    drop(pending);
    slot.close();
    assert!(slot.0.lock().unwrap().task.is_none());
    assert!(!slot.enqueue(Task::new(&json!({}), |_| {}).unwrap(), &tx, &closed));
}

#[test]
fn failed_wake_send_releases_pending_task() {
    let slot = Arc::new(Slot::default());
    let closed = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel();
    drop(rx);
    assert!(!slot.enqueue(Task::new(&json!({}), |_| {}).unwrap(), &tx, &closed));
    let pending = slot.0.lock().unwrap();
    assert!(pending.closed);
    assert!(!pending.queued);
    assert!(pending.task.is_none());
}

#[test]
fn shared_worker_coalesces_while_busy_and_keeps_event_admission_independent() {
    let server = MockServer::start();
    let flags = server.mock(|when, then| {
        when.method(POST)
            .path("/flags/")
            .query_param("v", "2")
            .json_body(json!({"distinct_id":"first"}));
        then.status(200)
            .json_body(json!({"featureFlags":{"ready":true}}));
    });
    let last = server.mock(|when, then| {
        when.method(POST)
            .path("/flags/")
            .json_body(json!({"distinct_id":"last-person"}));
        then.status(200).json_body(json!({"flags":{}}));
    });
    let events = server.mock(|when, then| {
        when.method(POST).path(Endpoint::Capture.path());
        then.status(200);
    });
    let handle =
        TransportHandle::spawn_stateful(options(server.base_url()), None, 1024 * 1024).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    assert!(handle.schedule_flags(
        Task::new(&json!({"distinct_id":"first"}), move |result| {
            assert!(result.is_ok());
            entered_tx.send(thread::current().id()).unwrap();
            release_rx.recv_timeout(WAIT).unwrap();
        })
        .unwrap()
    ));
    assert_eq!(
        entered_rx.recv_timeout(WAIT).unwrap(),
        handle.worker_id.unwrap()
    );
    let (done_tx, done_rx) = mpsc::channel();
    for n in 0..1000 {
        let done_tx = done_tx.clone();
        let id = if n == 999 {
            "last-person".to_string()
        } else {
            format!("discarded-{n}")
        };
        assert!(handle.schedule_flags(
            Task::new(&json!({"distinct_id":id}), move |result| {
                done_tx.send((n, result.is_ok())).unwrap();
            })
            .unwrap()
        ));
    }
    assert_eq!(
        handle.enqueue_prepared(wire("while-flags-busy")),
        Admission::Accepted
    );
    assert_eq!(handle.pending(), 1);
    release_tx.send(()).unwrap();
    assert_eq!(done_rx.recv_timeout(WAIT).unwrap(), (999, true));
    handle.flush_blocking();
    handle.shutdown_blocking();
    flags.assert_calls(1);
    last.assert_calls(1);
    events.assert_calls(1);
    assert!(done_rx.try_recv().is_err());
}

#[test]
fn close_discards_pending_but_allows_active_completion_and_reentrant_shutdown() {
    let server = MockServer::start();
    let flags = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({"flags":{}}));
    });
    let handle =
        Arc::new(TransportHandle::spawn_stateful(options(server.base_url()), None, 4096).unwrap());
    let weak = Arc::downgrade(&handle);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    assert!(handle.schedule_flags(
        Task::new(&json!({}), move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(WAIT).unwrap();
            weak.upgrade().unwrap().shutdown_blocking();
            done_tx.send(()).unwrap();
        })
        .unwrap()
    ));
    entered_rx.recv_timeout(WAIT).unwrap();
    let (pending_tx, pending_rx) = mpsc::channel();
    assert!(handle.schedule_flags(
        Task::new(&json!({}), move |_| {
            let _ = pending_tx.send(());
        })
        .unwrap()
    ));
    handle.begin_close();
    assert!(!handle.schedule_flags(Task::new(&json!({}), |_| {}).unwrap()));
    release_tx.send(()).unwrap();
    done_rx.recv_timeout(WAIT).unwrap();
    // begin_close is the split lifecycle primitive; explicitly send its control.
    let (tx, rx) = mpsc::channel();
    handle.send_control(Control::Shutdown {
        completion: super::super::Completion::Blocking(tx),
        deadline: None,
    });
    rx.recv_timeout(WAIT).unwrap();
    handle.join();
    assert!(pending_rx.try_recv().is_err());
    flags.assert_calls(1);
}

#[test]
fn callback_panic_does_not_stop_worker_and_server_lane_has_no_flag_slot() {
    let server = MockServer::start();
    let flags = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({"flags":{}}));
    });
    let handle = TransportHandle::spawn_stateful(options(server.base_url()), None, 4096).unwrap();
    assert!(handle
        .schedule_flags(Task::new(&json!({}), |_| panic!("controlled completion panic")).unwrap()));
    handle.flush_blocking();
    let (tx, rx) = mpsc::channel();
    assert!(handle.schedule_flags(
        Task::new(&json!({}), move |result| {
            tx.send(result.is_ok()).unwrap();
        })
        .unwrap()
    ));
    assert!(rx.recv_timeout(WAIT).unwrap());
    handle.shutdown_blocking();
    flags.assert_calls(2);
    let server_lane = TransportHandle::spawn(options(server.base_url()));
    assert!(!server_lane.schedule_flags(Task::new(&json!({}), |_| {}).unwrap()));
    server_lane.shutdown_blocking();
}

#[test]
fn request_wire_is_exact_and_diagnostics_omit_response_bodies() {
    let server = MockServer::start();
    let payload = json!({"api_key":"phc_test","distinct_id":"person", "$anon_distinct_id":"device", "$device_id":"device",
        "groups":{"company":"a"},"person_properties":{"plan":"pro"},"group_properties":{"company":{"tier":2}},"geoip_disable":true});
    let flags = server.mock(|when, then| {
        when.method(POST)
            .path("/flags/")
            .query_param("v", "2")
            .header("content-type", "application/json")
            .json_body(payload.clone());
        then.status(503).body("private server response");
    });
    let (errors_tx, errors_rx) = mpsc::channel();
    let mut settings = options(server.base_url());
    settings
        .on_error
        .push(crate::client::OnErrorHook::new(move |error| {
            errors_tx.send(format!("{error:?}")).unwrap();
        }));
    let handle = TransportHandle::spawn_stateful(settings, None, 4096).unwrap();
    let (tx, rx) = mpsc::channel();
    assert!(handle.schedule_flags(
        Task::new(&payload, move |result| {
            tx.send(result.err()).unwrap();
        })
        .unwrap()
    ));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Some(LoadError::Http(503)));
    let diagnostic = errors_rx.recv_timeout(WAIT).unwrap();
    assert!(!diagnostic.contains("private server response"));
    handle.shutdown_blocking();
    flags.assert_calls(1);
}

#[test]
fn flags_retry_policy_reuses_existing_status_rules_and_same_request_body() {
    for (status, attempts) in [(502, 2), (504, 2), (503, 1)] {
        let server = MockServer::start();
        let payload = json!({"distinct_id":"same-person", "groups":{"company":"a"}});
        let flags = server.mock(|when, then| {
            when.method(POST)
                .path("/flags/")
                .query_param("v", "2")
                .json_body(payload.clone());
            then.status(status);
        });
        let mut settings = options(server.base_url());
        settings.feature_flags_request_max_retries = 1;
        let handle = TransportHandle::spawn_stateful(settings, None, 4096).unwrap();
        let (tx, rx) = mpsc::channel();
        assert!(handle.schedule_flags(
            Task::new(&payload, move |result| {
                tx.send(result.err()).unwrap();
            })
            .unwrap()
        ));
        assert_eq!(
            rx.recv_timeout(WAIT).unwrap(),
            Some(LoadError::Http(status))
        );
        handle.shutdown_blocking();
        flags.assert_calls(attempts);
    }
}

#[test]
fn response_stream_has_a_byte_limit() {
    let server = MockServer::start();
    let flags = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).body(" ".repeat(MAX_RESPONSE_BYTES + 1));
    });
    let handle = TransportHandle::spawn_stateful(options(server.base_url()), None, 4096).unwrap();
    let (tx, rx) = mpsc::channel();
    assert!(handle.schedule_flags(
        Task::new(&json!({}), move |result| {
            tx.send(result.err()).unwrap();
        })
        .unwrap()
    ));
    assert_eq!(
        rx.recv_timeout(WAIT).unwrap(),
        Some(LoadError::ResponseTooLarge)
    );
    handle.shutdown_blocking();
    flags.assert_calls(1);
}

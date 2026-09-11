//! Producer-backed flag metadata on real wire requests, shared by all client/protocol builds.
use httpmock::{HttpMockRequest, HttpMockResponse, MockServer};
use posthog_rs::{ClientOptionsBuilder, EvaluateFlagsOptions, Event};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[cfg(feature = "async-client")]
macro_rules! client_call {
    ($call:expr) => {
        $call.await
    };
}
#[cfg(not(feature = "async-client"))]
macro_rules! client_call {
    ($call:expr) => {
        $call
    };
}

macro_rules! exercise_flag_wire {
    () => {{
        let server = MockServer::start();
        let flags = server.mock(|when, then| {
            when.method("POST").path("/flags/");
            then.status(200).json_body(json!({
                "flags": {"plain": {"key":"plain", "enabled":false,
                    "metadata":{"id":1,"version":1,"has_experiment":false}}},
                "minimalFlagCalledEvents":true, "errorsWhileComputingFlags":true
            }));
        });
        let bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
        let received = bodies.clone();
        let wire = server.mock(|when, then| {
            #[cfg(feature = "capture-v1")]
            when.method("POST").path("/i/v1/analytics/events");
            #[cfg(not(feature = "capture-v1"))]
            when.method("POST").path("/batch/");
            then.respond_with(move |request: &HttpMockRequest| {
                let body: Value = serde_json::from_slice(request.body_ref()).unwrap();
                let mut results = serde_json::Map::new();
                for event in body["batch"].as_array().unwrap() {
                    results.insert(event["uuid"].as_str().unwrap().to_string(), json!({"result":"ok"}));
                }
                received.lock().unwrap().push(body);
                HttpMockResponse::builder().status(200)
                    .header("content-type", "application/json")
                    .body(json!({"results":results}).to_string()).build()
            });
        });
        let options = ClientOptionsBuilder::default()
            .api_key("test-key".to_string()).host(server.base_url())
            .flush_interval_ms(600_000u64).flush_at(100usize).max_capture_attempts(1u32)
            .before_send(|mut event| {
                for key in ["customNull", "$feature/other", "$feature_flag_response_extra"] {
                    event.insert_prop(key, Value::Null).unwrap();
                }
                event.insert_prop("customKeep", true).unwrap();
                event.insert_prop("items", json!([null, {"drop":null}])).unwrap();
                Some(event)
            }).build().unwrap();
        let client = client_call!(posthog_rs::client(options));
        let snapshot = client_call!(client.evaluate_flags("test-user", EvaluateFlagsOptions::default())).unwrap();
        assert!(!snapshot.is_enabled("missing"));
        assert!(!snapshot.is_enabled("plain"));
        let mut ordinary = Event::new("ordinary", "test-user");
        ordinary.insert_prop("$feature_flag", "missing").unwrap();
        ordinary.insert_prop("$feature_flag_response", Value::Null).unwrap();
        ordinary.insert_prop("$feature/missing", Value::Null).unwrap();
        client.capture(ordinary);
        client_call!(client.flush());
        client_call!(client.shutdown());
        flags.assert_calls(1);
        wire.assert_calls(1);
        let bodies = bodies.lock().unwrap();
        let events: Vec<_> = bodies.iter().flat_map(|b| b["batch"].as_array().unwrap()).collect();
        assert_eq!(events.len(), 3);
        for e in events {
            let p = &e["properties"];
            for key in ["customNull", "$feature/other", "$feature_flag_response_extra"] {
                assert!(p.get(key).is_none(), "custom key retained: {}", e);
            }
            if e["event"] == "ordinary" {
                assert!(p.get("$feature_flag_response").is_none());
                assert!(p.get("$feature/missing").is_none());
            } else if p["$feature_flag"] == "missing" {
                assert_eq!(p.get("$feature_flag_response"), Some(&Value::Null), "generated response missing: {}", e);
                assert_eq!(p.get("$feature/missing"), Some(&Value::Null), "exact feature null missing: {}", e);
                assert_eq!(p["$feature_flag_error"], "errors_while_computing_flags,flag_missing");
                assert_eq!(p["items"], json!([null, {}]));
                assert_eq!(p["customKeep"], true);
            } else {
                assert_eq!(p["$feature_flag"], "plain");
                assert_eq!(p["$feature_flag_response"], false);
                for key in ["$feature/plain", "items", "customKeep"] {
                    assert!(p.get(key).is_none(), "minimal privacy bypassed: {}", e);
                }
            }
        }
    }};
}

#[cfg(not(feature = "async-client"))]
#[test]
fn flag_property_serialization_wire() {
    exercise_flag_wire!();
}
#[cfg(feature = "async-client")]
#[tokio::test]
async fn flag_property_serialization_wire() {
    exercise_flag_wire!();
}

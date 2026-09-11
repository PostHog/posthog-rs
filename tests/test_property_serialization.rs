//! Real loopback wire coverage shared by blocking/async and v0/v1 builds.
use httpmock::{HttpMockRequest, HttpMockResponse, MockServer};
use posthog_rs::{ClientOptionsBuilder, Event};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Serialize)]
struct CustomProperties {
    drop: Option<String>,
    keep: u64,
}
fn event(name: &str) -> Event {
    let mut event = Event::new(name, "test-user");
    for (key, value) in json!({
        "test": null, "nested": {"drop":null},
        "items":["1",null,2,{"drop":null},[null]],
        "emptyObject":{}, "emptyArray":[], "empty":"", "zero":0,
        "enabled":false, "literal":"null", "literalUndefined":"undefined",
        "$set":{"drop":null}, "$group_set":{"drop":null}
    })
    .as_object()
    .unwrap()
    {
        event.insert_prop(key, value).unwrap();
    }
    event.insert_prop("optionNone", None::<String>).unwrap();
    event
        .insert_prop(
            "struct",
            CustomProperties {
                drop: None,
                keep: u64::MAX,
            },
        )
        .unwrap();
    event
        .insert_prop("optionArray", vec![None::<String>])
        .unwrap();
    event
}
fn assert_properties(properties: &Value) {
    for key in ["test", "optionNone", "missing", "hookNull"] {
        assert!(
            properties.get(key).is_none(),
            "{} must be absent: {}",
            key,
            properties
        );
    }
    for (key, expected) in json!({
        "nested":{}, "items":["1",null,2,{},[null]],
        "emptyObject":{}, "emptyArray":[], "empty":"", "zero":0,
        "enabled":false, "literal":"null", "literalUndefined":"undefined",
        "$set":{}, "$group_set":{}, "optionArray":[null],
        "struct":{"keep":u64::MAX}, "hookItems":[null,{}]
    })
    .as_object()
    .unwrap()
    {
        assert_eq!(properties.get(key), Some(expected), "{}", key);
    }
}

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

macro_rules! exercise_routes {
    () => {{
        let server = MockServer::start();
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
            .flush_interval_ms(600_000u64).flush_at(100usize)
            .max_capture_attempts(1u32)
            .before_send(|mut event| {
                if event.event_name() == "drop" { return None; }
                event.insert_prop("hookNull", Value::Null).unwrap();
                event.insert_prop("hookItems", json!([null,{"drop":null}])).unwrap();
                Some(event)
            }).build().unwrap();
        let client = client_call!(posthog_rs::client(options));
        let original = event("queued");
        client.capture(original.clone());
        assert_eq!(original.properties().get("test"), Some(&Value::Null));
        assert_eq!(original.properties()["nested"], json!({"drop":null}));
        client.capture_batch(vec![event("batch"), event("$ai_generation")], false);
        client.capture(Event::new("drop", "test-user"));
        let mut only = Event::new("only", "test-user");
        only.insert_prop("test", Value::Null).unwrap();
        client.capture(only.clone());
        #[cfg(feature = "error-tracking")]
        {
            let error = std::io::Error::other("synthetic test error");
            let options = posthog_rs::CaptureExceptionOptions::default()
                .property("test", None::<String>).unwrap()
                .property("nested", json!({"drop":null})).unwrap()
                .property("items", json!([null,{"drop":null}])).unwrap();
            client_call!(client.capture_exception_with(&error, options)).unwrap();
        }
        client_call!(client.flush());
        client_call!(client.capture_immediate(event("immediate"))).unwrap();
        client_call!(client.capture_batch_immediate(vec![event("batch_immediate")], false)).unwrap();
        client_call!(client.capture_immediate(only)).unwrap();
        client_call!(client.capture_immediate(Event::new("drop", "test-user"))).unwrap();
        client_call!(client.shutdown());
        wire.assert_calls(4);
        let bodies = bodies.lock().unwrap();
        let events: Vec<_> = bodies.iter().flat_map(|b| b["batch"].as_array().unwrap()).collect();
        let expected_count = if cfg!(feature = "error-tracking") { 8 } else { 7 };
        assert_eq!(events.len(), expected_count);
        for e in events {
            let p = &e["properties"];
            assert!(p.get("test").is_none(), "null retained: {}", e);
            assert!(p.get("hookNull").is_none());
            assert_eq!(p["hookItems"], json!([null,{}]));
            match e["event"].as_str().unwrap() {
                "only" => {},
                "$exception" => {
                    assert_eq!(p["nested"], json!({}));
                    assert_eq!(p["items"], json!([null,{}]));
                    assert!(!p["$exception_list"].as_array().unwrap().is_empty());
                },
                "drop" => panic!("hook drop bypassed"),
                _ => assert_properties(p),
            }
        }
    }};
}

#[cfg(not(feature = "async-client"))]
#[test]
fn event_property_serialization_wire() {
    exercise_routes!();
}
#[cfg(feature = "async-client")]
#[tokio::test]
async fn event_property_serialization_wire() {
    exercise_routes!();
}

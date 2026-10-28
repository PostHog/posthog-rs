use posthog_rs::{Event, RequestContext};

#[test]
fn explicit_context_builds_events_without_replacing_explicit_values() {
    let context = RequestContext::new()
        .distinct_id("request-user")
        .session_id("request-session")
        .property("route", "/work")
        .unwrap()
        .option("cookieless_mode", true)
        .unwrap()
        .option("disable_skew_correction", true)
        .unwrap();
    let event = context.event("work");
    let value = serde_json::to_value(event).unwrap();
    assert_eq!(value["distinct_id"], "request-user");
    assert_eq!(value["properties"]["$session_id"], "request-session");
    assert_eq!(value["properties"]["route"], "/work");
    assert_eq!(value["options"]["cookieless_mode"], true);
    assert_eq!(value["options"]["disable_skew_correction"], true);

    let mut event = Event::new("work", "explicit-user");
    event
        .insert_prop("$session_id", "explicit-session")
        .unwrap();
    event.insert_prop("route", "/explicit").unwrap();
    event.insert_option("cookieless_mode", false).unwrap();
    // A null option counts as unset, so the context fills it.
    event
        .insert_option("disable_skew_correction", serde_json::Value::Null)
        .unwrap();
    context.apply_to_event(&mut event);
    let value = serde_json::to_value(event).unwrap();
    assert_eq!(value["distinct_id"], "explicit-user");
    assert_eq!(value["properties"]["$session_id"], "explicit-session");
    assert_eq!(value["properties"]["route"], "/explicit");
    assert_eq!(value["options"]["cookieless_mode"], false);
    assert_eq!(value["options"]["disable_skew_correction"], true);
}

#[test]
fn missing_context_identity_keeps_personless_capture() {
    let value =
        serde_json::to_value(RequestContext::new().session_id("session").event("work")).unwrap();
    assert!(!value["distinct_id"].as_str().unwrap().is_empty());
    assert_eq!(value["options"]["process_person_profile"], false);
    assert_eq!(value["properties"]["$session_id"], "session");
    let value = serde_json::to_value(RequestContext::new().event("work")).unwrap();
    assert!(value["properties"].get("$session_id").is_none());
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn concurrent_and_nested_contexts_do_not_escape_their_scope() {
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for identity in ["first", "second"] {
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            RequestContext::new()
                .distinct_id(identity)
                .scope(async {
                    barrier.wait().await;
                    assert_eq!(
                        RequestContext::current().unwrap().get_distinct_id(),
                        Some(identity)
                    );
                    RequestContext::new()
                        .distinct_id("nested")
                        .scope(async {
                            tokio::task::yield_now().await;
                            assert_eq!(
                                RequestContext::current().unwrap().get_distinct_id(),
                                Some("nested")
                            );
                        })
                        .await;
                    assert_eq!(
                        RequestContext::current().unwrap().get_distinct_id(),
                        Some(identity)
                    );
                    assert!(tokio::spawn(async { RequestContext::current() })
                        .await
                        .unwrap()
                        .is_none());
                })
                .await;
            assert!(RequestContext::current().is_none());
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert!(RequestContext::current().is_none());
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn dropped_and_panicking_requests_leave_no_context() {
    for panic in [false, true] {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(
            RequestContext::new()
                .distinct_id("request")
                .scope(async move {
                    started_tx.send(()).unwrap();
                    if panic {
                        panic!("application failure");
                    }
                    std::future::pending::<()>().await;
                }),
        );
        started_rx.await.unwrap();
        if !panic {
            task.abort();
        }
        assert!(task.await.is_err());
        assert!(RequestContext::current().is_none());
    }
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn queued_batch_and_exception_context_survives_scope_end() {
    use httpmock::prelude::*;
    use serde_json::{json, Value};
    let server = MockServer::start();
    let capture = server.mock(|when, then| {
        when.method(POST).is_true(|request| {
            let body: Value = serde_json::from_slice(request.body_ref()).unwrap();
            let Some(events) = body["batch"].as_array() else {
                return false;
            };
            !events.is_empty()
                && events.iter().all(|event| {
                    let identity = match event["event"].as_str() {
                        Some("work") => "explicit-user",
                        Some("batch") => "batch-user",
                        Some("$exception") => "context-user",
                        _ => return false,
                    };
                    event["session_id"] == "original-session"
                        && event["distinct_id"] == identity
                        && event["options"]["cookieless_mode"] == true
                })
        });
        then.status(200).json_body(json!({"results": {}}));
    });
    let client = posthog_rs::client(
        posthog_rs::ClientOptionsBuilder::default()
            .api_key("phc_test".into())
            .host(server.base_url())
            .flush_at(100usize)
            .build()
            .unwrap(),
    )
    .await;
    RequestContext::new()
        .distinct_id("context-user")
        .session_id("original-session")
        .option("cookieless_mode", true)
        .unwrap()
        .scope(async {
            client.capture(Event::new("work", "explicit-user"));
            client.capture_batch(vec![Event::new("batch", "batch-user")], true);
            #[cfg(feature = "error-tracking")]
            client
                .capture_exception(&std::io::Error::other("operation failed"))
                .await
                .unwrap();
        })
        .await;
    RequestContext::new()
        .session_id("unrelated-session")
        .scope(async {
            client.flush().await;
        })
        .await;
    capture.assert_calls(2);
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn flag_snapshot_keeps_originating_context_after_request_ends() {
    use httpmock::prelude::*;
    use serde_json::{json, Value};
    let server = MockServer::start();
    let flags_request = server.mock(|when, then| {
        when.method(POST).path("/flags/").is_true(|request| {
            serde_json::from_slice::<Value>(request.body_ref()).unwrap()["distinct_id"]
                == "flag-user"
        });
        then.status(200)
            .json_body(json!({"featureFlags": {"feature": true}}));
    });
    let capture = server.mock(|when, then| {
        when.method(POST).is_true(|request| {
            let body: Value = serde_json::from_slice(request.body_ref()).unwrap();
            body["batch"][0]["event"] == "$feature_flag_called"
                && body["batch"][0]["session_id"] == "original-session"
                && body["batch"][0]["properties"]
                    .get("other-request")
                    .is_none()
        });
        then.status(200).json_body(json!({"results": {}}));
    });
    let client = posthog_rs::client(("phc_test", server.base_url().as_str())).await;
    let context = RequestContext::new()
        .distinct_id("flag-user")
        .session_id("original-session");
    let flags = client
        .evaluate_flags_with_context(&context, Default::default())
        .await
        .unwrap();
    RequestContext::new()
        .session_id("unrelated-session")
        .property("other-request", true)
        .unwrap()
        .scope(async {
            assert!(flags.is_enabled("feature"));
        })
        .await;
    client.flush().await;
    flags_request.assert_calls(1);
    capture.assert_calls(1);
    let empty = client
        .evaluate_flags_with_context(&RequestContext::new(), Default::default())
        .await
        .unwrap();
    assert!(!empty.is_enabled("feature"));
    client.flush().await;
    flags_request.assert_calls(1);
    capture.assert_calls(1);
}

use posthog_rs::{Event, RequestContext};

#[test]
fn explicit_context_builds_events_without_replacing_explicit_values() {
    let context = RequestContext::new()
        .distinct_id("request-user")
        .session_id("request-session")
        .property("route", "/work")
        .unwrap();
    let event = context.event("work");
    let value = serde_json::to_value(event).unwrap();
    assert_eq!(value["distinct_id"], "request-user");
    assert_eq!(value["properties"]["$session_id"], "request-session");
    assert_eq!(value["properties"]["route"], "/work");

    let mut event = Event::new("work", "explicit-user");
    event
        .insert_prop("$session_id", "explicit-session")
        .unwrap();
    event.insert_prop("route", "/explicit").unwrap();
    context.apply_to_event(&mut event);
    let value = serde_json::to_value(event).unwrap();
    assert_eq!(value["distinct_id"], "explicit-user");
    assert_eq!(value["properties"]["$session_id"], "explicit-session");
    assert_eq!(value["properties"]["route"], "/explicit");
}

#[test]
fn missing_context_identity_keeps_personless_capture() {
    let value =
        serde_json::to_value(RequestContext::new().session_id("session").event("work")).unwrap();
    assert!(!value["distinct_id"].as_str().unwrap().is_empty());
    assert_eq!(value["properties"]["$process_person_profile"], false);
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

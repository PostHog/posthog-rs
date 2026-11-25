//! Custom events a tool captures through the host's client carry the session
//! and identity of the call that captured them.

#![cfg(feature = "mcp")]

mod mcp_support;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use mcp_support::http::{call_params, returned_handle, HttpServer};
use mcp_support::recorder::{text_property, Recorder};
use mcp_support::server::TestServer;
use posthog::mcp::{Identity, McpAnalytics, McpOptions, McpOptionsBuilder};
use posthog::{Event, RequestContext};
use serde_json::{json, Value};

fn custom_event_server(recorder: &Recorder, options: McpOptions) -> (HttpServer, TestServer) {
    let server = TestServer::with_custom_events(recorder.client.clone());
    let analytics = McpAnalytics::new(recorder.client.clone(), options);
    (
        HttpServer::stateless(analytics.instrument(server.clone())),
        server,
    )
}

fn search(query: &str) -> Value {
    call_params("search_docs", json!({ "query": query }))
}

/// Each custom event, keyed by its query, with the `$mcp_tool_call` or
/// failed call of the same query.
async fn by_query(recorder: &Recorder) -> Vec<(String, Event, Event)> {
    let calls = recorder.named("$mcp_tool_call").await;
    let custom = recorder.named("docs_searched").await;
    assert_eq!(custom.len(), calls.len());
    custom
        .into_iter()
        .map(|event| {
            let query = text_property(&event, "query").to_string();
            let call = calls
                .iter()
                .find(|call| {
                    call.properties()["$mcp_parameters"]
                        .to_string()
                        .contains(&query)
                })
                .expect("the call that captured it")
                .clone();
            (query, event, call)
        })
        .collect()
}

fn assert_same_session(query: &str, custom: &Event, call: &Event) {
    for key in ["$session_id", "$mcp_session_source"] {
        assert_eq!(
            text_property(custom, key),
            text_property(call, key),
            "{} of {}",
            key,
            query
        );
    }
}

#[tokio::test]
async fn a_custom_event_keeps_its_calls_session_while_another_call_runs() {
    let recorder = Recorder::new().await;
    let (http, server) = custom_event_server(
        &recorder,
        McpOptionsBuilder::default()
            .conversation_id(false)
            .build()
            .unwrap(),
    );
    let token = URL_SAFE_NO_PAD.encode(json!({ "sid": "ses_first" }).to_string());

    let replayed = [("mcp-session-id", token.as_str())];
    let first = http.request("tools/call", search("first"), &replayed);
    let second = async {
        server.release("second");
        http.request("tools/call", search("second"), &[]).await;
        server.release("first");
    };
    tokio::join!(first, second);

    for (query, custom, call) in by_query(&recorder).await {
        assert_same_session(&query, &custom, &call);
        if query == "first" {
            assert_eq!(text_property(&custom, "$session_id"), "ses_first");
        }
    }
}

#[tokio::test]
async fn a_custom_event_waits_for_its_calls_handle_to_be_delivered() {
    for query in ["delivered", "fail-search"] {
        let recorder = Recorder::new().await;
        let (http, server) = custom_event_server(&recorder, McpOptions::default());

        server.release(query);
        let host = RequestContext::new().session_id("ses_from_host_header");
        let response = http
            .request_in_context("tools/call", search(query), &[], host)
            .await;

        assert_eq!(returned_handle(&response).is_some(), query == "delivered");
        let matched = by_query(&recorder).await;
        assert_eq!(matched.len(), 1);
        let (_, custom, call) = &matched[0];
        assert_same_session(query, custom, call);
    }
}

#[tokio::test]
async fn a_batch_and_work_that_outlives_the_call_take_its_final_session() {
    for anchoring in [true, false] {
        let recorder = Recorder::new().await;
        let options = McpOptionsBuilder::default()
            .conversation_id(anchoring)
            .build()
            .unwrap();
        let (http, server) = custom_event_server(&recorder, options);

        server.release("spread");
        http.request("tools/call", search("spread"), &[]).await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let call = recorder.only("$mcp_tool_call").await;
        for name in ["docs_batched", "docs_followed_up", "docs_nested"] {
            let custom = recorder.only(name).await;
            assert_same_session(name, &custom, &call);
        }
        let nested = recorder.only("docs_nested").await;
        assert_eq!(text_property(&nested, "step"), "nested");
    }
}

#[tokio::test]
async fn a_custom_event_carries_its_calls_identity() {
    let recorder = Recorder::new().await;
    let options = McpOptionsBuilder::default()
        .identify(|request| {
            let query = request.arguments?.get("query")?.as_str()?;
            Some(Identity::new(format!("user-{query}")).with_group("company", "acme"))
        })
        .build()
        .unwrap();
    let (http, server) = custom_event_server(&recorder, options);

    for query in ["ada", "grace"] {
        server.release(query);
        http.request("tools/call", search(query), &[]).await;
    }

    for (query, custom, call) in by_query(&recorder).await {
        assert_eq!(custom.distinct_id(), format!("user-{query}"));
        assert_eq!(custom.properties()["$groups"], json!({ "company": "acme" }));
        assert_same_session(&query, &custom, &call);
    }
}

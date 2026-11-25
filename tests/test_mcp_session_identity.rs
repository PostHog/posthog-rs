//! The spec's "Session identity" scenarios: client conversation ids, the
//! session resolver, and the source every event names for its session.

#![cfg(feature = "mcp")]

mod mcp_support;

use mcp_support::http::{
    call_params, modern_meta, result_texts, returned_handle, HttpResponse, HttpServer,
    MODERN_PROTOCOL,
};
use mcp_support::recorder::{text_property, Recorder};
use mcp_support::server::TestServer;
use posthog::mcp::{McpAnalytics, McpOptions, McpOptionsBuilder};
use serde_json::{json, Value};

const CHATGPT: &str = "openai-mcp/1.0.0 (ChatGPT)";
const CODEX: &str = "codex-mcp-client/0.50.0";
const CHATGPT_SESSION: &str = "ses_2f5d35b7108d17c807766f2f769a1e8c";
const CODEX_SESSION: &str = "ses_0fdf126b8e2c1f9689ca12f3dd3383e6";
const VSCODE_SESSION: &str = "ses_0571acb894e6753d33e37e5c2b327c59";
const HANDLE: &str = "0190f0e8-7a6b-7c3d-9e4f-5a6b7c8d9e0f";

fn anchoring(enabled: bool) -> McpOptions {
    McpOptionsBuilder::default()
        .conversation_id(enabled)
        .build()
        .unwrap()
}

fn stateless(recorder: &Recorder, server: TestServer, options: McpOptions) -> HttpServer {
    let analytics = McpAnalytics::new(recorder.client.clone(), options);
    HttpServer::stateless(analytics.instrument(server))
}

async fn call(
    http: &HttpServer,
    arguments: Value,
    meta: Value,
    user_agent: Option<&str>,
) -> HttpResponse {
    let mut params = call_params("search_docs", arguments);
    params["_meta"] = meta;
    let headers = match user_agent {
        Some(user_agent) => vec![("user-agent", user_agent)],
        None => vec![("mcp-protocol-version", MODERN_PROTOCOL)],
    };
    http.request("tools/call", params, &headers).await
}

fn has_handle_block(response: &HttpResponse) -> bool {
    result_texts(response)
        .iter()
        .any(|text| text.contains("conversation_id"))
}

#[tokio::test]
async fn a_client_conversation_id_sets_the_session_and_adds_no_handle() {
    let codex_uppercase = json!({ "sessionId": "019A4F2E-1C3D-7B8A-9E0F-1A2B3C4D5E6F" });
    let mut vscode = modern_meta("Visual Studio Code", "1.99.0");
    vscode["vscode.conversationId"] = json!("3f1c2b7e-8d4a-4c6b-9a1e-5f2d7c8b9a0e");
    let cases = [
        (
            "chatgpt",
            Some(CHATGPT),
            json!({ "openai/session": "v1/conv-example-0001" }),
            CHATGPT_SESSION,
        ),
        ("codex", Some(CODEX), codex_uppercase, CODEX_SESSION),
        (
            "vscode on 2026-07-28, by client name",
            None,
            vscode,
            VSCODE_SESSION,
        ),
    ];
    for anchoring_enabled in [true, false] {
        for (case, user_agent, meta, expected) in cases.clone() {
            let recorder = Recorder::new().await;
            let http = stateless(&recorder, TestServer::new(), anchoring(anchoring_enabled));

            let response = call(&http, json!({ "query": "flags" }), meta, user_agent).await;

            assert!(!has_handle_block(&response), "{}", case);
            let event = recorder.only("$mcp_tool_call").await;
            assert_eq!(text_property(&event, "$session_id"), expected, "{case}");
            assert_eq!(
                text_property(&event, "$mcp_session_source"),
                "client_metadata",
                "{case}"
            );
            assert!(
                !event.properties().contains_key("$mcp_conversation_id"),
                "{}",
                case
            );
        }
    }
}

#[tokio::test]
async fn a_client_conversation_id_wins_over_a_handle_and_is_never_sent() {
    let recorder = Recorder::new().await;
    let server = TestServer::new();
    let http = stateless(&recorder, server.clone(), McpOptions::default());

    let arguments = json!({ "query": "flags", "conversation_id": HANDLE });
    let meta = json!({ "openai/session": "v1/conv-example-0001" });
    call(&http, arguments, meta, Some(CHATGPT)).await;

    assert_eq!(
        server.received("search_docs"),
        json!({ "query": "flags" }).as_object().cloned()
    );
    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(text_property(&event, "$session_id"), CHATGPT_SESSION);
    assert!(!event.properties().contains_key("$mcp_conversation_id"));
    let sent = recorder.sent("$mcp_tool_call").await.to_string();
    assert!(!sent.contains("conv-example-0001"), "{}", sent);
}

#[tokio::test]
async fn an_unusable_client_id_falls_back_by_its_clients_handle_rule() {
    let shared = json!({
        "sessionId": "019a4f2e-1c3d-7b8a-9e0f-1a2b3c4d5e6f",
        "openai/session": "v1/conv-example-0001",
    });
    let cases = [
        (
            "invalid codex id",
            Some(CODEX),
            json!({ "sessionId": "not-a-uuid" }),
            true,
        ),
        (
            "nil uuid",
            Some(CODEX),
            json!({ "sessionId": "00000000-0000-0000-0000-000000000000" }),
            true,
        ),
        (
            "unrecognized client",
            Some("example-client/1.0"),
            shared,
            true,
        ),
        (
            "empty chatgpt id",
            Some(CHATGPT),
            json!({ "openai/session": "" }),
            false,
        ),
        (
            "non-ascii chatgpt id",
            Some(CHATGPT),
            json!({ "openai/session": "conv-é" }),
            false,
        ),
        ("chatgpt without id", Some(CHATGPT), json!({}), false),
    ];
    for (case, user_agent, meta, gets_handle) in cases {
        let recorder = Recorder::new().await;
        let http = stateless(&recorder, TestServer::new(), McpOptions::default());

        let first = call(&http, json!({ "query": "flags" }), meta.clone(), user_agent).await;
        let second = call(&http, json!({ "query": "flags" }), meta, user_agent).await;

        assert_eq!(returned_handle(&first).is_some(), gets_handle, "{case}");
        assert_eq!(has_handle_block(&second), gets_handle, "{case}");
        let events = recorder.named("$mcp_tool_call").await;
        let sessions: Vec<&str> = events
            .iter()
            .map(|event| text_property(event, "$session_id"))
            .collect();
        assert_ne!(sessions[0], sessions[1], "{case}");
        for session in &sessions {
            assert!(
                ![CHATGPT_SESSION, CODEX_SESSION].contains(session),
                "{}",
                case
            );
        }
        let source = if gets_handle {
            "conversation"
        } else {
            "generated"
        };
        assert_eq!(
            text_property(&events[0], "$mcp_session_source"),
            source,
            "{case}"
        );
    }
}

#[tokio::test]
async fn a_session_resolver_outranks_every_other_source() {
    let cases = [
        ("over a client id", false, "search_docs", Some(CHATGPT)),
        ("over an undelivered handle", true, "query", None),
    ];
    for (case, anchoring_enabled, tool, user_agent) in cases {
        let recorder = Recorder::new().await;
        let options = McpOptionsBuilder::default()
            .conversation_id(anchoring_enabled)
            .session_resolver(|_request| Some("tenant-42".to_string()))
            .build()
            .unwrap();
        let http = stateless(&recorder, TestServer::new(), options);

        let mut params = call_params(tool, json!({}));
        params["_meta"] = json!({ "openai/session": "v1/conv-example-0001" });
        let headers: Vec<_> = user_agent
            .map(|ua| ("user-agent", ua))
            .into_iter()
            .collect();
        http.request("tools/call", params, &headers).await;

        let event = recorder.only("$mcp_tool_call").await;
        let session = text_property(&event, "$session_id");
        assert_eq!(session, "ses_49f24de273ad14bdc3c6e16273c81fd9", "{case}");
        assert_eq!(
            text_property(&event, "$mcp_session_source"),
            "host",
            "{case}"
        );
        let handle = event.properties().get("$mcp_conversation_id");
        assert!(handle.is_none(), "{}", case);
    }
}

#[tokio::test]
async fn a_resolver_that_does_not_answer_falls_through() {
    let unanswered: [fn() -> Option<String>; 3] = [
        || None,
        || Some(String::new()),
        || panic!("resolver failed"),
    ];
    for resolve in unanswered {
        let recorder = Recorder::new().await;
        let options = McpOptionsBuilder::default()
            .session_resolver(move |_request| resolve())
            .build()
            .unwrap();
        let http = stateless(&recorder, TestServer::new(), options);

        let meta = json!({ "openai/session": "v1/conv-example-0001" });
        call(&http, json!({ "query": "flags" }), meta, Some(CHATGPT)).await;

        let event = recorder.only("$mcp_tool_call").await;
        assert_eq!(text_property(&event, "$session_id"), CHATGPT_SESSION);
        assert_eq!(
            text_property(&event, "$mcp_session_source"),
            "client_metadata"
        );
    }
}

#[tokio::test]
async fn only_a_handle_free_clients_listing_leaves_out_conversation_id() {
    let cases = [
        (CHATGPT, false),
        ("example-client/1.0", true),
        (CODEX, true),
    ];
    for (user_agent, advertised) in cases {
        let recorder = Recorder::new().await;
        let http = stateless(&recorder, TestServer::new(), McpOptions::default());

        let response = http
            .request("tools/list", json!({}), &[("user-agent", user_agent)])
            .await;

        let tools = response.body["result"]["tools"].as_array().unwrap();
        let search = tools
            .iter()
            .find(|tool| tool["name"] == "search_docs")
            .unwrap();
        let properties = &search["inputSchema"]["properties"];
        assert!(properties.get("context").is_some(), "{}", user_agent);
        assert_eq!(
            properties.get("conversation_id").is_some(),
            advertised,
            "{}",
            user_agent
        );
    }
}

#[tokio::test]
async fn a_handle_free_listing_does_not_stop_others_echoing_handles() {
    let recorder = Recorder::new().await;
    let server = TestServer::new();
    let http = stateless(&recorder, server.clone(), McpOptions::default());

    http.request("tools/list", json!({}), &[("user-agent", CHATGPT)])
        .await;
    let arguments = json!({ "query": "flags", "conversation_id": HANDLE });
    call(&http, arguments, json!({}), Some("example-client/1.0")).await;

    assert_eq!(
        server.received("search_docs"),
        json!({ "query": "flags" }).as_object().cloned()
    );
    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(text_property(&event, "$mcp_conversation_id"), HANDLE);
}

#[tokio::test]
async fn every_event_names_the_source_of_its_session() {
    let recorder = Recorder::new().await;
    let http = HttpServer::stateful(
        McpAnalytics::new(recorder.client.clone(), McpOptions::default())
            .instrument(TestServer::new()),
    );
    let initialize = json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": { "name": "cursor", "version": "1.4.0" },
    });
    let opened = http.request("initialize", initialize, &[]).await;
    let session = opened.session_id.expect("a stateful session");
    let headers = [
        ("mcp-session-id", session.as_str()),
        ("mcp-protocol-version", "2025-06-18"),
    ];
    http.notify("notifications/initialized", &headers).await;
    http.request("tools/list", json!({}), &headers).await;
    http.request("tools/call", call_params("query", json!({})), &headers)
        .await;

    for name in ["$mcp_tools_list", "$mcp_tool_call", "$exception"] {
        let event = recorder.only(name).await;
        assert_eq!(
            text_property(&event, "$mcp_session_source"),
            "transport",
            "{name}"
        );
    }
}

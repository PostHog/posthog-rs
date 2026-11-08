//! MCP analytics over Streamable HTTP, where only a conversation handle or a
//! replayed session id ties a client's requests together.

#![cfg(feature = "mcp")]

mod mcp_support;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use mcp_support::http::{
    call_params, modern_meta, result_texts, returned_handle, HttpServer, MODERN_PROTOCOL,
};
use mcp_support::recorder::{property, text_property, Recorder};
use mcp_support::server::TestServer;
use posthog::mcp::{Identity, McpAnalytics, McpOptions, McpOptionsBuilder};
use posthog::RequestContext;
use serde_json::{json, Value};

const HANDLE: &str = "0190f0e8-7a6b-7c3d-9e4f-5a6b7c8d9e0f";
const HANDLE_SESSION: &str = "ses_6df45f0102a182bcd5e8dd5dad6c65a0";

fn instrumented(
    recorder: &Recorder,
    server: TestServer,
    options: McpOptions,
) -> posthog::mcp::Instrumented<TestServer> {
    McpAnalytics::new(recorder.client.clone(), options).instrument(server)
}

fn stateless(recorder: &Recorder, server: TestServer) -> HttpServer {
    HttpServer::stateless(instrumented(recorder, server, McpOptions::default()))
}

#[tokio::test]
async fn a_conversation_handle_keeps_a_stateless_clients_calls_in_one_session() {
    let recorder = Recorder::new().await;
    let http = stateless(&recorder, TestServer::new());

    let first = http
        .request(
            "tools/call",
            call_params("search_docs", json!({ "query": "flags" })),
            &[],
        )
        .await;
    let handle = returned_handle(&first).expect("the first result ends with a handle");
    assert_eq!(result_texts(&first)[0], "Found 3 results");
    let arguments = json!({ "query": "flags", "conversation_id": handle });
    let second = http
        .request("tools/call", call_params("search_docs", arguments), &[])
        .await;

    assert_eq!(
        result_texts(&second),
        vec!["Found 3 results"],
        "an echoed handle is not repeated"
    );
    let events = recorder.named("$mcp_tool_call").await;
    assert_eq!(events.len(), 2);
    for event in &events {
        assert_eq!(text_property(event, "$mcp_conversation_id"), handle);
    }
    assert_eq!(
        text_property(&events[0], "$session_id"),
        text_property(&events[1], "$session_id")
    );
}

#[tokio::test]
async fn a_conversation_handle_derives_the_canonical_session_id() {
    let recorder = Recorder::new().await;
    let http = stateless(&recorder, TestServer::new());

    let arguments = json!({ "query": "flags", "conversation_id": HANDLE });
    http.request("tools/call", call_params("search_docs", arguments), &[])
        .await;

    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(text_property(&event, "$session_id"), HANDLE_SESSION);
}

#[tokio::test]
async fn a_malformed_handle_is_replaced_and_never_reaches_the_tool() {
    let recorder = Recorder::new().await;
    let server = TestServer::new();
    let http = stateless(&recorder, server.clone());

    let arguments = json!({ "query": "flags", "conversation_id": "not-a-uuid" });
    let response = http
        .request("tools/call", call_params("search_docs", arguments), &[])
        .await;

    assert!(returned_handle(&response).is_some());
    assert_eq!(
        server.received("search_docs"),
        json!({ "query": "flags" }).as_object().cloned()
    );
}

#[tokio::test]
async fn clients_without_a_session_or_handle_never_share_a_session() {
    let recorder = Recorder::new().await;
    let options = McpOptionsBuilder::default()
        .conversation_id(false)
        .build()
        .unwrap();
    let http = HttpServer::stateless(instrumented(&recorder, TestServer::new(), options));

    for _client in ["A", "B"] {
        let response = http
            .request(
                "tools/call",
                call_params("search_docs", json!({ "query": "flags" })),
                &[],
            )
            .await;
        assert_eq!(result_texts(&response), vec!["Found 3 results"]);
    }

    let events = recorder.named("$mcp_tool_call").await;
    assert_eq!(events.len(), 2);
    assert_ne!(
        text_property(&events[0], "$session_id"),
        text_property(&events[1], "$session_id")
    );
}

#[tokio::test]
async fn a_modern_request_identifies_its_client_from_meta() {
    let recorder = Recorder::new().await;
    let http = stateless(&recorder, TestServer::new());

    let mut params = call_params("search_docs", json!({ "query": "flags" }));
    params["_meta"] = modern_meta("claude-code", "2.1.0");
    let headers = [
        ("mcp-protocol-version", MODERN_PROTOCOL),
        ("user-agent", "claude-code/2.1.0 (cli)"),
    ];
    http.request("tools/call", params, &headers).await;

    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(text_property(&event, "$mcp_client_name"), "claude-code");
    assert_eq!(text_property(&event, "$mcp_client_version"), "2.1.0");
    assert_eq!(
        text_property(&event, "$mcp_protocol_version"),
        MODERN_PROTOCOL
    );
    assert_eq!(
        text_property(&event, "$mcp_client_user_agent"),
        "claude-code/2.1.0 (cli)"
    );
}

#[tokio::test]
async fn a_model_named_in_client_metadata_wins() {
    let recorder = Recorder::new().await;
    let http = stateless(&recorder, TestServer::new());

    let mut params = call_params(
        "search_docs",
        json!({ "query": "flags", "llm_model": "self-claimed" }),
    );
    let mut meta = modern_meta("codex", "1.0.0");
    meta["x-codex-turn-metadata"] = json!({ "model": "gpt-5.2" });
    params["_meta"] = meta;
    http.request(
        "tools/call",
        params,
        &[("mcp-protocol-version", MODERN_PROTOCOL)],
    )
    .await;

    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(text_property(&event, "$mcp_llm_model"), "gpt-5.2");
    assert_eq!(
        text_property(&event, "$mcp_llm_model_source"),
        "client_metadata"
    );
}

#[tokio::test]
async fn a_round_asking_for_input_is_recorded_and_the_completed_call_counted_once() {
    let recorder = Recorder::new().await;
    let http = stateless(&recorder, TestServer::new());
    let headers = [("mcp-protocol-version", MODERN_PROTOCOL)];

    let mut params = call_params("deploy", json!({}));
    params["_meta"] = modern_meta("claude-code", "2.1.0");
    let round = http.request("tools/call", params.clone(), &headers).await;
    assert_eq!(
        round.body["result"]["resultType"], "input_required",
        "{}",
        round.body
    );

    assert!(recorder.named("$mcp_tool_call").await.is_empty());
    let input_required = recorder.only("$mcp_input_required").await;
    assert_eq!(text_property(&input_required, "$mcp_tool_name"), "deploy");
    assert_eq!(
        property(&input_required, "$mcp_input_request_methods"),
        &json!(["elicitation/create"])
    );
    assert!(property(&input_required, "$mcp_duration_ms").is_number());

    params["inputResponses"] =
        json!({ "confirm": { "action": "accept", "content": { "confirm": true } } });
    let completed = http.request("tools/call", params, &headers).await;
    assert_eq!(result_texts(&completed)[0], "deployed");
    let call = recorder.only("$mcp_tool_call").await;
    assert!(!call.properties()["$mcp_parameters"]
        .to_string()
        .contains("inputResponses"));
}

#[tokio::test]
async fn a_stateful_session_groups_calls_by_its_id() {
    let recorder = Recorder::new().await;
    let http = HttpServer::stateful(instrumented(
        &recorder,
        TestServer::new(),
        McpOptions::default(),
    ));

    let initialize = json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": { "name": "cursor", "version": "1.4.0" },
    });
    let initialized = http.request("initialize", initialize, &[]).await;
    let session_id = initialized
        .session_id
        .expect("a stateful server assigns a session");
    let headers = [
        ("mcp-session-id", session_id.as_str()),
        ("mcp-protocol-version", "2025-06-18"),
    ];
    http.notify("notifications/initialized", &headers).await;
    for query in ["a", "b"] {
        let response = http
            .request(
                "tools/call",
                call_params("search_docs", json!({ "query": query })),
                &headers,
            )
            .await;
        assert_eq!(
            result_texts(&response),
            vec!["Found 3 results"],
            "no handle within a session"
        );
    }

    let events = recorder.named("$mcp_tool_call").await;
    assert_eq!(events.len(), 2);
    for event in &events {
        assert_eq!(
            text_property(event, "$session_id"),
            text_property(&events[0], "$session_id")
        );
        assert_eq!(text_property(event, "$mcp_client_name"), "cursor");
        assert_eq!(text_property(event, "$mcp_protocol_version"), "2025-06-18");
    }
    assert!(recorder
        .only("$mcp_initialize")
        .await
        .properties()
        .contains_key("$mcp_client_name"));
}

#[tokio::test]
async fn a_replayed_posthog_session_token_keeps_its_session_and_client() {
    let recorder = Recorder::new().await;
    let http = stateless(&recorder, TestServer::new());
    let token_payload =
        json!({ "sid": "ses_from_token", "cn": "claude-code", "cv": "2.1.0", "pv": "2025-06-18" });
    let token = URL_SAFE_NO_PAD.encode(token_payload.to_string());

    http.request(
        "tools/call",
        call_params("search_docs", json!({ "query": "flags" })),
        &[("mcp-session-id", token.as_str())],
    )
    .await;

    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(text_property(&event, "$session_id"), "ses_from_token");
    assert_eq!(text_property(&event, "$mcp_client_name"), "claude-code");
    assert!(!event.properties().contains_key("$mcp_conversation_id"));
}

#[tokio::test]
async fn a_minted_handle_is_mirrored_for_tools_that_declare_it() {
    let recorder = Recorder::new().await;
    let tool = mcp_support::server::tool(json!({
        "name": "count",
        "inputSchema": { "type": "object", "properties": {} },
        "outputSchema": { "type": "object", "properties": { "count": { "type": "number" } } },
    }));
    let http = stateless(&recorder, TestServer::with_tools(vec![tool]));

    let listing = http.request("tools/list", json!({}), &[]).await;
    let advertised = &listing.body["result"]["tools"][0]["outputSchema"]["properties"];
    assert!(
        advertised.get("_mcp_instructions").is_some(),
        "{}",
        listing.body
    );
    let response = http
        .request("tools/call", call_params("count", json!({})), &[])
        .await;

    let handle = returned_handle(&response).expect("a minted handle");
    let structured: &Value = &response.body["result"]["structuredContent"];
    assert!(
        structured.is_null()
            || structured["_mcp_instructions"]["conversation_id"] == handle.as_str()
    );
}

#[tokio::test]
async fn a_call_naming_no_tool_sends_nothing() {
    let recorder = Recorder::new().await;
    let http = stateless(&recorder, TestServer::new());

    http.request("tools/call", call_params("", json!({})), &[])
        .await;

    assert!(recorder.events().await.is_empty());
}

#[tokio::test]
async fn a_synthesized_stateless_handshake_names_no_client() {
    let recorder = Recorder::new().await;
    let http = stateless(&recorder, TestServer::new());

    let headers = [("mcp-session-id", "a-gateway-session-id")];
    http.request(
        "tools/call",
        call_params("search_docs", json!({ "query": "flags" })),
        &headers,
    )
    .await;

    let event = recorder.only("$mcp_tool_call").await;
    assert!(
        !event.properties().contains_key("$mcp_client_name"),
        "{:?}",
        event.properties()
    );
    assert!(text_property(&event, "$session_id").starts_with("ses_"));
}

/// Analytics defaults an application attaches to the HTTP request, setting
/// every capture option.
fn request_context() -> RequestContext {
    RequestContext::new()
        .distinct_id("context-user")
        .session_id("context-session")
        .property("deployment", "canary")
        .unwrap()
        .option("cookieless_mode", true)
        .unwrap()
        .option("disable_skew_correction", true)
        .unwrap()
        .option("product_tour_id", "tour-1")
        .unwrap()
        .option("process_person_profile", true)
        .unwrap()
}

fn identified(recorder: &Recorder) -> HttpServer {
    let options = McpOptionsBuilder::default()
        .identify(|_| Some(Identity::new("user-123")))
        .build()
        .unwrap();
    HttpServer::stateless(instrumented(recorder, TestServer::new(), options))
}

fn failing_call() -> Value {
    call_params("fetch_page", json!({ "url": "https://example.com" }))
}

/// The context's property and options reach `name` on the wire. The MCP
/// session and distinct id win, and `process_person_profile` is `person`.
async fn assert_sent_with_context(
    recorder: &Recorder,
    name: &str,
    distinct_id: &str,
    person: bool,
) {
    let sent = recorder.sent(name).await;
    let session_id = sent["session_id"].as_str().unwrap();
    assert!(session_id.starts_with("ses_"), "{}: {}", name, session_id);
    let expected_distinct_id = if distinct_id.is_empty() {
        session_id
    } else {
        distinct_id
    };
    assert_eq!(sent["distinct_id"], expected_distinct_id, "{name}");
    assert_eq!(sent["properties"]["deployment"], "canary", "{name}");
    assert_eq!(
        sent["options"],
        json!({
            "cookieless_mode": true,
            "disable_skew_correction": true,
            "product_tour_id": "tour-1",
            "process_person_profile": person,
        }),
        "{name}"
    );
}

#[tokio::test]
async fn the_requests_context_fills_every_event_and_the_mcp_identity_wins() {
    let recorder = Recorder::new().await;
    let http = identified(&recorder);

    http.request_in_context("tools/call", failing_call(), &[], request_context())
        .await;

    for name in ["$mcp_tool_call", "$exception"] {
        assert_sent_with_context(&recorder, name, "user-123", true).await;
    }
}

#[tokio::test]
async fn an_anonymous_call_stays_personless_under_a_context_that_processes_persons() {
    let recorder = Recorder::new().await;
    let http = stateless(&recorder, TestServer::new());

    http.request_in_context("tools/call", failing_call(), &[], request_context())
        .await;

    for name in ["$mcp_tool_call", "$exception"] {
        assert_sent_with_context(&recorder, name, "", false).await;
    }
}

#[tokio::test]
async fn the_requests_context_fills_listing_and_handshake_events() {
    let recorder = Recorder::new().await;
    let http = stateless(&recorder, TestServer::new());

    let initialize = json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": { "name": "agent", "version": "1.0" },
    });
    http.request_in_context("initialize", initialize, &[], request_context())
        .await;
    http.request_in_context("tools/list", json!({}), &[], request_context())
        .await;

    for name in ["$mcp_initialize", "$mcp_tools_list"] {
        assert_sent_with_context(&recorder, name, "", false).await;
    }
}

#[tokio::test]
async fn an_mcp_hook_sees_the_requests_context_and_its_changes_are_sent() {
    let recorder = Recorder::new().await;
    let options = McpOptionsBuilder::default()
        .before_send(|mut event| {
            assert_eq!(event.properties()["deployment"], "canary");
            event.remove_prop("deployment");
            event.remove_option("disable_skew_correction");
            Some(event)
        })
        .build()
        .unwrap();
    let http = HttpServer::stateless(instrumented(&recorder, TestServer::new(), options));

    http.request_in_context(
        "tools/call",
        call_params("search_docs", json!({ "query": "flags" })),
        &[],
        request_context(),
    )
    .await;

    let sent = recorder.sent("$mcp_tool_call").await;
    assert!(sent["properties"].get("deployment").is_none(), "{:#}", sent);
    assert!(
        sent["options"].get("disable_skew_correction").is_none(),
        "{:#}",
        sent
    );
    assert_eq!(sent["options"]["product_tour_id"], "tour-1");
}

/// The same request context, set by `PostHogLayer` from the request.
#[cfg(feature = "tower")]
mod posthog_layer {
    use super::*;

    const HEADERS: [(&str, &str); 2] = [
        ("x-posthog-distinct-id", "header-user"),
        ("x-posthog-session-id", "header-session"),
    ];

    async fn assert_request_described(recorder: &Recorder) {
        let sent = recorder.sent_properties("$mcp_tool_call").await;
        assert_eq!(sent["$request_method"], "POST");
        assert_eq!(sent["$request_path"], "/mcp");
    }

    #[tokio::test]
    async fn the_layers_context_fills_every_event_and_the_mcp_identity_wins() {
        let recorder = Recorder::new().await;
        let http = identified(&recorder);

        http.request_through_posthog_layer(
            "tools/call",
            failing_call(),
            &HEADERS,
            Some(request_context()),
        )
        .await;

        for name in ["$mcp_tool_call", "$exception"] {
            assert_sent_with_context(&recorder, name, "user-123", true).await;
        }
        assert_request_described(&recorder).await;
    }

    #[tokio::test]
    async fn an_anonymous_call_stays_personless_behind_the_layer() {
        let recorder = Recorder::new().await;
        let http = stateless(&recorder, TestServer::new());

        http.request_through_posthog_layer(
            "tools/call",
            failing_call(),
            &HEADERS,
            Some(request_context()),
        )
        .await;

        for name in ["$mcp_tool_call", "$exception"] {
            assert_sent_with_context(&recorder, name, "", false).await;
        }
        assert_request_described(&recorder).await;
    }
}

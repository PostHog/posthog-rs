//! The manual MCP analytics API, for servers analytics cannot wrap.

#![cfg(feature = "mcp")]

mod mcp_support;

use mcp_support::http::{call_params, HttpServer};
use mcp_support::recorder::{property, text_property, Recorder};
use mcp_support::server::TestServer;
use posthog_rs::mcp::{
    Connection, McpAnalytics, McpOptions, ToolCallError, ToolCallRequest, ToolRegistration,
    TransportSession,
};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool};
use rmcp::ServerHandler;
use serde_json::json;

#[derive(Debug)]
struct UpstreamTimeout;

impl std::fmt::Display for UpstreamTimeout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("upstream timed out")
    }
}

impl std::error::Error for UpstreamTimeout {}

fn arguments(value: serde_json::Value) -> Option<JsonObject> {
    Some(serde_json::from_value(value).unwrap())
}

fn search_docs() -> Tool {
    TestServer::new().get_tool("search_docs").unwrap()
}

#[tokio::test]
async fn a_manually_captured_call_matches_the_automatic_one() {
    let recorder = Recorder::new().await;
    let analytics = McpAnalytics::new(recorder.client.clone(), McpOptions::default());
    let call_arguments = json!({ "query": "flags", "context": "find the flags docs" });

    let http = HttpServer::stateless(analytics.instrument(TestServer::new()));
    http.request(
        "tools/call",
        call_params("search_docs", call_arguments.clone()),
        &[],
    )
    .await;
    let manual = analytics.start_tool_call(
        ToolCallRequest::new("search_docs", arguments(call_arguments))
            .tool(ToolRegistration::Registered(search_docs())),
    );
    assert_eq!(
        manual.arguments(),
        arguments(json!({ "query": "flags" })).as_ref()
    );
    manual.complete(CallToolResult::success(vec![ContentBlock::text(
        "Found 3 results",
    )]));

    let events = recorder.named("$mcp_tool_call").await;
    assert_eq!(events.len(), 2);
    for key in [
        "$mcp_tool_name",
        "$mcp_intent",
        "$mcp_parameters",
        "$mcp_is_error",
    ] {
        assert_eq!(
            property(&events[0], key),
            property(&events[1], key),
            "{}",
            key
        );
    }
    assert!(text_property(&events[1], "$session_id").starts_with("ses_"));
}

#[tokio::test]
async fn an_explicit_error_type_labels_the_call_but_not_the_exception() {
    let recorder = Recorder::new().await;
    let analytics = McpAnalytics::new(recorder.client.clone(), McpOptions::default());

    let call = analytics.start_tool_call(
        ToolCallRequest::new("search_docs", arguments(json!({ "query": "flags" })))
            .tool(ToolRegistration::Registered(search_docs()))
            .transport_session(TransportSession::Connection(Connection::new())),
    );
    call.fail(ToolCallError::new(&UpstreamTimeout).error_type("validation"));

    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(text_property(&event, "$mcp_error_type"), "validation");
    assert_eq!(
        text_property(&event, "$mcp_error_message"),
        "upstream timed out"
    );
    let exception = recorder.only("$exception").await;
    assert_eq!(
        property(&exception, "$exception_list")[0]["type"],
        "UpstreamTimeout"
    );
}

#[tokio::test]
async fn a_thrown_errors_type_names_the_failure() {
    let recorder = Recorder::new().await;
    let analytics = McpAnalytics::new(recorder.client.clone(), McpOptions::default());

    let call = analytics.start_tool_call(ToolCallRequest::new("query", None));
    call.fail(ToolCallError::new(&UpstreamTimeout));

    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(text_property(&event, "$mcp_error_type"), "UpstreamTimeout");
}

#[tokio::test]
async fn a_call_to_an_unregistered_tool_is_recorded_as_unknown() {
    let recorder = Recorder::new().await;
    let analytics = McpAnalytics::new(recorder.client.clone(), McpOptions::default());

    let call = analytics.start_tool_call(
        ToolCallRequest::new("no_such_tool", arguments(json!({})))
            .tool(ToolRegistration::NotRegistered),
    );
    call.fail(ToolCallError::new(&UpstreamTimeout));

    assert!(recorder.named("$mcp_tool_call").await.is_empty());
    let event = recorder.only("$mcp_unknown_tool").await;
    assert_eq!(text_property(&event, "$mcp_tool_name"), "no_such_tool");
}

#[tokio::test]
async fn listing_advertises_the_analytics_arguments() {
    let recorder = Recorder::new().await;
    let analytics = McpAnalytics::new(recorder.client.clone(), McpOptions::default());

    let tools = analytics.list_tools(vec![search_docs()]);

    let schema = serde_json::to_value(&tools[0].input_schema).unwrap();
    for argument in ["context", "llm_model", "conversation_id"] {
        assert_eq!(
            schema["properties"][argument]["type"], "string",
            "{}",
            argument
        );
    }
    assert_eq!(
        search_docs()
            .input_schema
            .get("properties")
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        1
    );
}
